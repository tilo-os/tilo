//! Workspaces this relay governs.
//!
//! A shared workspace needs one authority that decides who is in it: it
//! issues invitations, admits the device that redeems one, changes roles and
//! removes members, and publishes each decision as signed workspace governance
//! every member verifies. Here that authority is the relay hosting the workspace.
//! Its own device is the workspace's governor: it keeps the workspace's [`StatePeer`],
//! publishes governance as its own retained changes of the workspace,
//! and hands the governed member set to the relay's admission after every
//! decision, so a member it admits can subscribe at once and a member it
//! removes is cut off.
//!
//! Workspaces are administered as their own `tilo.workspace` governance actions,
//! and the host's own operations (creating, listing and entering workspaces) as
//! `tilo.host` actions (`actions`), or, from a page, over HTTPS: by devices
//! of the relay's own user, and by other members as far as the workspace's
//! governance lets them (`authority`, one decision for every door).
//! Any device redeems an invitation over the shared workspace-join
//! protocol, and a page over HTTPS, where someone the workspace lets invite then
//! admits it.

mod actions;
mod admissions;
mod agent_door;
#[cfg(feature = "agents")]
mod agent_members;
pub mod app_files;
#[cfg(feature = "agents")]
mod applications;
mod authority;
mod channel_governance;
mod collaboration;
mod content;
#[cfg(any(test, feature = "test-support"))]
pub mod fixtures;
mod surfaces;
pub(crate) use content::lacks_answer;
pub use content::{ContentKeeper, WorkspaceContent};
mod desktop_writes;
mod device_keys;
mod device_plane;
#[cfg(test)]
mod device_plane_tests;
mod door;
mod follow;
pub use device_plane::HostJobs;
use follow::Followed;
#[cfg(test)]
mod device_rejoin_tests;
#[cfg(test)]
mod hard_stop_tests;
pub(crate) use desktop_writes::writes_desktop;
mod hosts;
mod listing;
mod own;
mod pages;
mod peer_parts;
mod publisher;
mod record_format;
mod revocations;
pub use own::OwnWorkspaceOffer;
#[cfg(any(feature = "agents", feature = "web"))]
pub(crate) mod sealed_records;
pub use device_keys::{
    ChannelKeys, ChannelKeysAnswer, ChannelKeysAnswerFuture, ChannelSink, DeviceChannels,
    DeviceKeys,
};
#[cfg(all(any(test, feature = "test-support"), feature = "agents"))]
pub use sealed_records::fixtures as sealed_fixtures;
mod desktop_record;
mod shares;
#[cfg(test)]
pub(crate) mod tests;
mod unbuildable_windows;
#[cfg(feature = "agents")]
mod views;
mod voice;
pub(crate) use voice::carried_call_room;
mod wait_list;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use tilo_peer_state::replica::PeerGuard;

use tilo_peer_state::direct::Replica;
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

pub(crate) use actions::{GovernedPlane, HostWorkspaceActions};
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{CollaborationError, GovernanceCompletion};
pub use listing::DoorRefusal;
pub use pages::PreAdmitRequest;
pub use wait_list::{Mailbox, Secrets as WaitListSecrets, WaitListed};

/// An invitation someone who may invite asked the workspace to mail to an
/// address on its wait list ([`Workspaces::mail_wait_list`]).
pub struct WaitListMail {
    /// Completed only after the operator durably accepts the mail.
    pub queued: tokio::sync::oneshot::Sender<bool>,
    pub to: Mailbox,
    /// The invitation's web link.
    pub link: String,
    /// A secret that takes the entry off the list, for the mail's removal
    /// link; the entry keeps only its SHA-256.
    pub removal: [u8; 32],
}
// The administration vocabulary (the actions' names and answers), under the
// daemon's historic paths.
pub use tilo_relay_session::workspaces::{
    ATTACH_HOST, ActionInvitation, CREATE_ROOM, CancelInvitationRequest, CreatedWorkspace,
    DETACH_HOST, ENTER_ROOM, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOM,
    HOST_ROOMS_RESOURCE, HostedWorkspace, IssueInvitationRequest, IssuedInvitation, LIST_ROOMS,
    PendingInvitation, RemoveMemberRequest, SetMemberRoleRequest, UNHOST_ROOM, WorkspaceListing,
    WorkspaceSummary, ask_host,
};

use crate::peer::WorkspaceRegistry;
use tilo_lock::RwLockRecover as _;

/// The record's version this build writes. Version 3 reads a version 2
/// record's grants in the invitation layout they were written in (see
/// [`record_format`]).
const RECORD_VERSION: u32 = 3;
/// Governance publications kept after the last full checkpoint before the
/// workspace's publication is compacted into a fresh one.
const MAX_PUBLICATION_TAIL: usize = 256;
/// How long members' writes the governor took in may wait to be written
/// through to the workspace file. A hard stop loses at most these; replaying
/// what the relay retains brings them back.
const KEEP_FOLLOWED: Duration = Duration::from_secs(1);
/// Record writes the workspace authority has yet to look at, per workspace.
const RECORD_NOTICES: usize = 256;

/// One governance message this workspace has published, in publication order.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Publication {
    bytes: Vec<u8>,
    checkpoint: bool,
}

/// Everything a governed workspace keeps across restarts. Written atomically
/// after each decision, before it is published or answered; the large parts
/// of `peer` are files of their own ([`peer_parts`]), written before it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorkspaceRecord {
    version: u32,
    workspace: String,
    workspace_id: u64,
    name: String,
    authority: DeviceCert,
    peer: Vec<peer_parts::PeerPart>,
    generation: u64,
    /// The complete committed publication: the last full checkpoint and every
    /// change after it. Members receive it over the workspace's sync.
    outgoing: Vec<Publication>,
    /// How much of `outgoing` the relay has retained, as of the workspace's last
    /// write. Publishing does not write the workspace again: after a crash the
    /// workspace republishes what it published since, which members tolerate.
    published: usize,
    collaboration: collaboration::CollaborationState,
}

/// What a governance change of a workspace is, as far as starting the workspace's
/// document over for newcomers goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    /// It only admits people ([`GovernedRoom::commit_admission`]).
    Admission,
    /// Anything else.
    Other,
}

/// A workspace this relay governs.
struct GovernedWorkspace {
    record: WorkspaceRecord,
    peer: Replica,
    path: PathBuf,
    /// The files of `record.peer`'s large parts.
    parts: peer_parts::PeerParts,
    /// The channel keys of the device this host runs on, asked for the
    /// records of this workspace: `None` when that device keeps no channel
    /// keys, and the workspace's records are then held in the clear.
    device: Option<DeviceKeys>,
    /// The owner's pre-admissions and notices.
    admissions: admissions::Admissions,
    /// The addresses people left at its door without an invitation.
    wait_list: wait_list::WaitList,
    /// Whom moderation removed, and when: the public door keeps them out for
    /// the day.
    door: door::Door,
    /// Admits the governance each settle retains to the host's order, after
    /// the workspace's lock is released.
    publisher: Arc<publisher::WorkspacePublisher>,
    /// Member writes to follow before compacting a sealed record history
    /// again, after a compaction failed: each attempt opens and merges the
    /// whole history.
    #[cfg(any(feature = "agents", feature = "web"))]
    compaction_wait: usize,
    /// The workspace's shared desktop as members' writes of it are decided,
    /// followed from the first such write ([`desktop_writes`]).
    desktop: Option<tilo_desktop_model::shared_record::guard::DesktopGuard>,
    /// The segments of members' larger desktop writes, until each is whole.
    desktop_segments: collab_types::DocumentReassembler,
    /// Members' writes the governor took in that the workspace file lacks.
    unkept: bool,
    /// Whether writing them through is due already ([`Workspaces::keep_soon`]).
    keeping: bool,
    /// The channels of this workspace's records whose participants this host
    /// is asking to admit it, by app and record: a burst of writes to a
    /// record the host holds no keys of yet asks once
    /// ([`Workspaces::join_soon`]).
    #[cfg(any(feature = "agents", feature = "web"))]
    asking: std::collections::BTreeSet<(String, String)>,
    /// The workspace's document started over from a base at an admission, and
    /// what the relay retains of it from before is still to be archived,
    /// ahead of the base's publication ([`Workspaces::settle`]).
    rebased: bool,
    /// Every change published since the workspace's document last started over
    /// at an admission was itself an admission ([`Self::commit_admission`]):
    /// a person let in, perhaps named by their own choice, and nothing
    /// changed or taken back. A newcomer may then be given that tail as it
    /// is, since it shows nothing withdrawn before they joined, and the
    /// workspace need not start over for them. Only kept in memory: a workspace read
    /// back from its file starts over at its next admission, as before.
    ///
    /// Held as the workspace document's heads when that was last true, so a
    /// write that reached the document any other way (a member's write the
    /// governor took in) leaves them behind, and the next admission starts
    /// over.
    admissions_since_base: Option<Vec<automerge::ChangeHash>>,
    /// The session each of those hosts last opened here that this host
    /// answered ([`Workspaces::answer_host`]), by the host's device.
    answered: HashMap<[u8; 32], hosts::Answer>,
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

    fn create(
        dir: &Path,
        name: &str,
        authority: &DeviceCert,
        key: &SigningKey,
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        Self::create_in(
            dir,
            &uuid::Uuid::new_v4().to_string(),
            name,
            (authority, key),
            device,
            now,
        )
    }

    /// A new governed workspace under the identity `workspace`: a fresh one, or the
    /// workspace a workspace already is when it moves here from the device that
    /// made it (a workspace is its workspace, and keeps it when its governance
    /// moves).
    fn create_in(
        dir: &Path,
        workspace: &str,
        name: &str,
        (authority, key): (&DeviceCert, &SigningKey),
        device: Option<&DeviceKeys>,
        now: SystemTime,
    ) -> Result<Self, String> {
        let name = valid_name(name)?;
        let workspace = uuid::Uuid::parse_str(workspace)
            .map_err(|_| "a room is a UUID".to_owned())?
            .hyphenated()
            .to_string();
        if dir.join(format!("{workspace}.room")).exists() {
            return Err("this host has that room already".into());
        }
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &workspace,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        let mut seed = tilo_peer_state::workspace_seed(&workspace, authority, &roster, name)?;
        if device.is_some() {
            // The workspace's records are sealed channels this node opens on
            // first use: a seed record would go out in the clear before its
            // channel exists, and every member who joins after the channel
            // opened refuses that cleartext. The shared desktop starts with
            // the first arrangement written to it, sealed, as in a workspace that
            // never had one (`prepare_desktop_record`).
            seed.documents.clear();
        }
        peer.install_seed(seed, key, now)?;
        prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .publication_mut()
            .durable_bootstrap_messages(key, now, 1)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let path = dir.join(format!("{workspace}.room"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = WorkspaceRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&workspace)
                .map_err(|error| error.to_string())?,
            workspace: workspace.clone(),
            name: name.to_owned(),
            authority: authority.clone(),
            peer: parts.stage(&peer)?,
            generation: 1,
            outgoing,
            published: 0,
            collaboration: collaboration::CollaborationState::default(),
        };
        let mut this = Self {
            admissions: admissions::Admissions::open(&path)?,
            wait_list: wait_list::WaitList::open(&path)?,
            door: door::Door::open(&path)?,
            parts,
            path,
            device: device.cloned(),
            record,
            peer: Replica::new(peer),
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            #[cfg(any(feature = "agents", feature = "web"))]
            asking: std::collections::BTreeSet::new(),
            rebased: false,
            admissions_since_base: None,
            answered: HashMap::new(),
        };
        this.persist()?;
        Ok(this)
    }

    fn open(
        path: PathBuf,
        authority: &DeviceCert,
        device: Option<&DeviceKeys>,
    ) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let (version, _) = postcard::take_from_bytes::<u32>(&bytes).map_err(|error| {
            format!(
                "{}: a workspace record names its version first: {error}",
                path.display()
            )
        })?;
        // An older record comes up before it is read: a version 2 record's
        // grants are in the invitation layout they were written in, which
        // [`record_format`] reads by the invitation's own version. A newer
        // one is left as it is and refused.
        let bytes = record_format::FORMAT
            .bring_up(&path, bytes, version)
            .map_err(|error| error.to_string())?;
        let record: WorkspaceRecord = postcard::from_bytes(&bytes).map_err(|error| {
            format!(
                "{}: {} version {version}: {error}",
                path.display(),
                record_format::FORMAT.name
            )
        })?;
        // The record must have been governed by this host's device. What
        // makes a device that device is its key and what the certificate
        // lets it do; a certificate's `device_label` is a human-readable
        // name, which `DeviceCert` itself calls "not authoritative", and a
        // host that renamed its own certificate — the same key, the same
        // capabilities, the same validity, a new label — still governs the
        // workspace it made. Refusing the label alone made a host that
        // renamed its certificate from "room host" to "workspace host" unable
        // to start on the workspaces it had been serving.
        if record.authority.device_public_key != authority.device_public_key
            || record.authority.user_public_key != authority.user_public_key
            || record.authority.capabilities != authority.capabilities
            || record.authority.expires_at_ns != authority.expires_at_ns
        {
            return Err("workspace is governed by a different device".into());
        }
        if !crate::room_workspace_id_matches(&record.workspace, record.workspace_id)
            || record.published > record.outgoing.len()
        {
            return Err("workspace record is inconsistent".into());
        }
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let mut peer = StatePeer::restore(
            &parts.load(&record.peer)?,
            &record.workspace,
            record.authority.clone(),
            vec![record.authority.clone()],
            RevocationList::new(),
            SystemTime::now(),
        )?;
        select_exact_admission(&mut peer)?;
        if !peer.has_governance() {
            return Err("restored room has no governance".into());
        }
        // Parts an interrupted write left behind.
        parts.collect(&record.peer);
        Ok(Self {
            admissions: admissions::Admissions::open(&path)?,
            wait_list: wait_list::WaitList::open(&path)?,
            door: door::Door::open(&path)?,
            record,
            peer: Replica::new(peer),
            parts,
            path,
            device: device.cloned(),
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
            unkept: false,
            keeping: false,
            #[cfg(any(feature = "agents", feature = "web"))]
            asking: std::collections::BTreeSet::new(),
            rebased: false,
            admissions_since_base: None,
            answered: HashMap::new(),
        })
    }

    fn persist(&mut self) -> Result<(), String> {
        // What the publisher could not admit is not published yet, whatever
        // `published` says until the next settle takes the failure back.
        let bytes = match self.publisher.unretained_from() {
            Some(first) if first < self.record.published => {
                let mut record = self.record.clone();
                record.published = first;
                postcard::to_stdvec(&record)
            }
            _ => postcard::to_stdvec(&self.record),
        }
        .map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())?;
        self.parts.collect(&self.record.peer);
        Ok(())
    }

    fn peer(&self) -> PeerGuard<'_> {
        self.peer.lock()
    }

    fn workspace(&self) -> &str {
        &self.record.workspace
    }

    const fn workspace_id(&self) -> u64 {
        self.record.workspace_id
    }

    fn collaboration(&self) -> &collaboration::CollaborationState {
        &self.record.collaboration
    }

    /// The signed invitation the workspace issued `person` that is still
    /// redeemable at `now_ns`: the door of a workspace that takes people by
    /// invitation hands that one back rather than minting one.
    fn invitation_for(&self, person: [u8; 32], now_ns: u64) -> Option<IssuedInvitation> {
        self.collaboration().invitation_for(person, now_ns)
    }

    /// Take off the wait list everyone who joined with an invitation sent
    /// for their entry. A failed write leaves them for the next time.
    fn forget_joined_wait_list(&mut self) {
        let ledger = &self.record.collaboration;
        match self
            .wait_list
            .joined(|invite| ledger.redeemed(invite), tilo_clock::unix_now_ns())
        {
            Ok(0) => {}
            Ok(count) => tracing::info!(
                workspace = %self.record.workspace,
                count,
                "invited addresses left the wait list on joining"
            ),
            Err(error) => tracing::warn!(
                workspace = %self.record.workspace,
                %error,
                "the wait list kept invited addresses that joined"
            ),
        }
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
            &WorkspaceRecord,
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
            &WorkspaceRecord,
            &StatePeer,
        ) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        let committing = crate::timing::start();
        if candidate.workspace_id() != replica.workspace_id()
            || candidate.owner() != replica.owner()
        {
            return Err("a room change cannot replace the room".into());
        }
        // A member admitted from now on is given the workspace as it stands, not
        // what was withdrawn before they joined.
        let joined = !member_roots(&candidate, now).is_subset(&member_roots(replica, now));
        let preparing = crate::timing::start();
        prepare_governance(&mut candidate, key, now)?;
        crate::timing::window("room.commit.prepare", preparing);
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or("workspace generation exhausted")?;
        let copying = crate::timing::start();
        let mut record = self.record.clone();
        crate::timing::window("room.commit.record", copying);
        let tail = record
            .outgoing
            .iter()
            .rev()
            .take_while(|publication| !publication.checkpoint)
            .count();
        let mut rebased = false;
        // Newcomers whose arrival only adds to a tail of earlier arrivals
        // see nothing in it that was withdrawn before them: the workspace's
        // document need not start over for them. Anything else since the
        // last start, or a long tail, starts it over as before.
        let appended_admission = joined
            && change == Change::Admission
            && tail < MAX_PUBLICATION_TAIL
            && self
                .admissions_since_base
                .as_ref()
                .is_some_and(|clean| replica.workspace_heads().as_ref() == Some(clean));
        let started_over = (tail >= MAX_PUBLICATION_TAIL || joined) && !appended_admission;
        if started_over {
            // Keep unchanged record checkpoints on their own channels; replacing
            // those baselines could discard member writes. A record changed by
            // this commit must still publish at the governance rollover.
            let changed_records =
                durable_messages(&mut candidate.clone(), replica, (key, now), generation)?
                    .into_iter()
                    .filter_map(|message| match message {
                        neighbour_wire::NeighbourMsg::AppDocument {
                            app_id, record_id, ..
                        } => Some((app_id, record_id)),
                        _ => None,
                    })
                    .collect::<std::collections::BTreeSet<_>>();
            rebased = joined && candidate.rebase_workspace()?;
            record.outgoing = candidate
                .publication_mut()
                .durable_bootstrap_messages(key, now, generation)?
                .iter()
                .filter(|message| match message {
                    neighbour_wire::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => changed_records.contains(&(app_id.clone(), record_id.clone())),
                    _ => true,
                })
                .filter(|message| for_members(message))
                .map(|message| publication(message, true))
                .collect::<Result<_, _>>()?;
            record.published = 0;
        } else {
            let diffing = crate::timing::start();
            let messages = durable_messages(&mut candidate, replica, (key, now), generation)?;
            for message in messages.iter().filter(|message| for_members(message)) {
                record.outgoing.push(publication(message, false)?);
            }
            crate::timing::window("room.commit.messages", diffing);
        }
        let encoding = crate::timing::start();
        record.peer = self.parts.stage(&candidate)?;
        crate::timing::window("room.commit.durable", encoding);
        record.generation = generation;
        let deriving = crate::timing::start();
        record.collaboration = state(&record, &candidate)?;
        crate::timing::window("room.commit.state", deriving);
        let prior = std::mem::replace(&mut self.record, record);
        let persisting = crate::timing::start();
        let persisted = self.persist();
        crate::timing::window("room.commit.persist", persisting);
        if let Err(error) = persisted {
            self.record = prior;
            return Err(error);
        }
        *replica = candidate;
        self.rebased |= rebased;
        // Clean from a start made at an admission; kept clean only by
        // admissions appended to it; anything else spoils it until the next.
        let clean = if started_over {
            joined
        } else {
            appended_admission
        };
        self.admissions_since_base = clean.then(|| replica.workspace_heads()).flatten();
        crate::timing::event(
            "room.governance_commit",
            committing,
            self.record.outgoing.len() as u64,
        );
        Ok(generation)
    }

    fn summary(&self) -> WorkspaceSummary {
        let now = SystemTime::now();
        let mut members = self
            .peer()
            .governance()
            .governed_members(now)
            .unwrap_or_default();
        // The structural owner is the workspace's own person: it holds the
        // workspace without an admission receipt, and every list and count of
        // its people includes it, from the authority pinned in the record.
        let authority = &self.record.authority;
        if !members
            .iter()
            .any(|member| member.user_public_key == authority.user_public_key)
        {
            members.insert(
                0,
                tilo_peer_state::GovernedMember {
                    user_public_key: authority.user_public_key,
                    role: tilo_peer_state::join::MemberRole::Editor,
                    display_name: Some(authority.device_label.clone()),
                    active_device_keys: Vec::new(),
                    removed: false,
                    administrator: true,
                },
            );
        }
        WorkspaceSummary {
            workspace: self.record.workspace.clone(),
            workspace_id: self.record.workspace_id,
            name: self.record.name.clone(),
            generation: self.record.generation,
            members,
        }
    }

    /// This workspace as a listing names it: what it is called and which
    /// generation it stands at, without the people a summary answers, which
    /// a listing does not.
    fn listing(&self) -> WorkspaceListing {
        WorkspaceListing {
            workspace: self.record.workspace.clone(),
            workspace_id: self.record.workspace_id,
            name: self.record.name.clone(),
            generation: self.record.generation,
        }
    }
}

use tilo_peer_state::workspace_membership::valid_workspace_name as valid_name;

/// Keep exact admission evidence in the governor's copy of the workspace. A
/// member of a workspace whose channels are sealed writes its records sealed,
/// and the governor follows those writes into its copy: with exact evidence
/// it keeps each one as the record's ciphertext history and acknowledges
/// it in its next governance change. Without it a sealed write only marks
/// the record encrypted, and every later change of the workspace (letting the
/// next person in among them) is refused as incomplete encryption state.
/// A workspace committed before this selection takes it on its next commit.
///
/// The copy also writes under the host's own actor
/// ([`tilo_peer_state::workspace_host_actor`]): a device hosting its own workspaces
/// never writes one workspace from two documents under one actor.
fn select_exact_admission(peer: &mut StatePeer) -> Result<(), String> {
    peer.write_as_workspace_host();
    peer.select_admission_v5()
        .map_err(|error| format!("workspace exceeds exact admission bounds: {error}"))
}

fn prepare_governance(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    peer.governance_mut()
        .reconcile_administrator_grants(key, now)?;
    peer.media_mut().reconcile_media_governance(key, now)?;
    peer.media().validate_media_governance_for_publication()
}

/// What `candidate` publishes for the workspace's records, as its changes
/// since the governor's copy in `peer`: the messages a commit sends on, and
/// the records one that starts the workspace over must still publish.
fn durable_messages(
    candidate: &mut StatePeer,
    peer: &StatePeer,
    (key, now): (&SigningKey, SystemTime),
    generation: u64,
) -> Result<Vec<neighbour_wire::NeighbourMsg>, String> {
    candidate
        .publication_mut()
        .durable_mutation_messages(peer, key, now, generation)
}

/// Whether the workspace publishes `message` of a commit to its members. The
/// governor's compaction of a record's sealed history bounds its own copy:
/// every member already holds each write it covers, and the relay retains
/// those writes for whoever joins later, so sending it on would push the
/// whole record to every member each time the history fills, for nothing.
/// It stays in the governor's durable copy alone.
fn for_members(message: &neighbour_wire::NeighbourMsg) -> bool {
    !matches!(
        message,
        neighbour_wire::NeighbourMsg::AppDocument { envelope, .. }
            if envelope.kind == collab_types::OpKind::Compaction
    )
}

fn publication(
    message: &neighbour_wire::NeighbourMsg,
    checkpoint: bool,
) -> Result<Publication, String> {
    let (_, kind) = neighbour_wire::retention(message, checkpoint)
        .ok_or("workspace governance message has no retention channel")?;
    Ok(Publication {
        bytes: postcard::to_stdvec(message).map_err(|error| error.to_string())?,
        checkpoint: kind == Retention::Checkpoint,
    })
}

/// The extension of the marker beside a workspace file this host released.
const RELEASED: &str = "released";

/// What the workspace authority needs from the relay hosting its workspaces.
#[derive(Clone)]
pub(crate) struct WorkspaceServices {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::host::GovernedAccessWriter,
    pub(crate) records: crate::workspace_records::WorkspaceRecords,
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: Arc<std::sync::RwLock<RevocationList>>,
    /// The host's metrics, which the sessions it dials count in too.
    pub(crate) metrics: Arc<crate::metrics::Metrics>,
    /// Pre-admissions each workspace keeps, spent ones included.
    pub(crate) max_pre_admissions: usize,
    /// The live handshake trust policy, for [`crate::workspace_access::decide`],
    /// which a host with agents of its own decides with.
    #[cfg_attr(
        not(feature = "agents"),
        expect(dead_code, reason = "only a host with agents of its own decides")
    )]
    pub(crate) trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// Agent delegations, for [`crate::workspace_access::decide`] and for
    /// refusing an agent the administration only a person holds.
    pub(crate) agents: crate::peer::AgentRegistry,
    /// What the join answers say the host is: a certified host, or the
    /// governing device serving its own workspaces.
    pub(crate) mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode,
    /// Each device whose session to this host authenticated: a host it
    /// dials and could not reach is dialled again then.
    pub(crate) arrivals: crate::attach::Arrivals,
    /// The channel service of the device this host runs on, when the
    /// embedder runs one: the host seals and opens the records of the
    /// workspaces it governs with its keys and admits nobody.
    pub(crate) channels: Option<DeviceChannels>,
}

/// The workspaces this relay governs.
#[derive(Clone)]
pub struct Workspaces {
    inner: Arc<WorkspacesInner>,
}

/// [`Workspaces`] for whoever must not keep them alive: the browser plane, which
/// hands sessions the calls of the workspaces this relay carries.
#[derive(Clone)]
pub struct WeakWorkspaces(std::sync::Weak<WorkspacesInner>);

impl WeakWorkspaces {
    /// The workspaces, while the relay still has them.
    #[must_use]
    pub fn upgrade(&self) -> Option<Workspaces> {
        self.0.upgrade().map(|inner| Workspaces { inner })
    }
}

struct WorkspacesInner {
    host: WorkspaceServices,
    /// The sealed history of every workspace governed here, which their
    /// member devices catch up from over the direct link.
    store: Arc<tilo_peer_state::store::Store>,
    /// The direct links of the workspaces governed here, one per workspace,
    /// answering a device's dial by the workspace it names.
    direct: tilo_peer_state::direct::Serving,
    dir: PathBuf,
    workspaces: Arc<device_plane::WorkspacesMap>,
    /// The plane the workspaces answer their members' devices on, as this
    /// host's own device ([`device_plane`]).
    plane: tilo_peer_state::actions::ActionPlane,
    /// The work this host runs for its workspaces' devices: none until an
    /// embedder hands it a job runner ([`Workspaces::run_jobs`]).
    jobs: Arc<device_plane::HostWork>,
    /// Held while a workspace is created: two devices placing the same workspace at
    /// once create it once.
    creating: AsyncMutex<()>,
    calls: Arc<voice::Calls>,
    /// The workspace UUID after each settled governance decision.
    changes: tokio::sync::broadcast::Sender<String>,
    /// Each record write of a member the governor's copy took in.
    record_writes: tokio::sync::broadcast::Sender<RecordWrite>,
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
    /// The workspaces listed as public workspaces, and how lively each is.
    listings: listing::Listings,
    /// Whether the records its workspaces hold in the clear are sealed: only once
    /// the host's operator decided it (the relay's `reseal`).
    #[cfg(feature = "agents")]
    resealing: std::sync::atomic::AtomicBool,
    /// The workspaces this host serves for the device of its person that governs
    /// them ([`crate::hosted`]).
    hosting: crate::hosted::Hosting,
    /// Where invitations to mail to wait-list addresses go, once whoever
    /// runs the relay sends mail ([`Workspaces::mail_wait_list`]).
    wait_list_mail: std::sync::OnceLock<(String, tokio::sync::mpsc::UnboundedSender<WaitListMail>)>,
    /// The workspaces of people's own, once the host's operator offers them
    /// ([`Workspaces::offer_own_workspaces`]).
    own: std::sync::OnceLock<own::OwnWorkspaces>,
    /// The devices the host's own person runs an agent on, and their open
    /// sessions ([`agent_door`]).
    agent_door: agent_door::AgentDoor,
}

/// Notices one workspace hands its channel service at once; a device asks
/// again when its request is dropped.
const MAX_QUEUED_CHANNEL_NOTICES: usize = 16_384;

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

/// A member's write to a record of a governed workspace, after the governor's
/// copy took it in: what wakes the workspace's installed bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordWrite {
    /// The workspace's UUID.
    pub workspace: String,
    /// The record's app.
    pub app: String,
    /// The record's id on the wire (`""` for the app's primary).
    pub record: String,
}

/// What [`Workspaces::refound_record`] did: which record was founded again,
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
    /// The workspaces, while the relay still governs them.
    pub fn workspaces(&self) -> Option<Workspaces> {
        self.workspaces.upgrade().map(|inner| Workspaces { inner })
    }

    /// The governed workspace.
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

/// `host`, serving with the certificate the workspace records under `dir`
/// name, when they are the same device: the record's authority is the one the
/// workspace was made with, and every comparison a host makes for its
/// workspace is exact. A record of another device — or one too old to read —
/// leaves the host's own, and opening refuses it as before.
fn host_serving(mut host: WorkspaceServices, dir: &Path) -> WorkspaceServices {
    let names = std::fs::read_dir(dir).ok().into_iter().flatten().flatten();
    for entry in names {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "room") {
            continue;
        }
        let Ok(authority) = record_authority(&path) else {
            continue;
        };
        if authority.device_public_key == host.cert.device_public_key
            && authority.user_public_key == host.cert.user_public_key
            && authority.capabilities == host.cert.capabilities
        {
            host.cert = authority;
        }
        break;
    }
    host
}

/// The certificate a workspace record names as its authority, read from the
/// record's own bytes: a version 2 or 3 record starts with the same five
/// fields, and the authority is the fifth.
fn record_authority(path: &Path) -> Result<DeviceCert, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let mut rest: &[u8] = &bytes;
    let (_, tail) = postcard::take_from_bytes::<u32>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) = postcard::take_from_bytes::<String>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) = postcard::take_from_bytes::<u64>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (_, tail) = postcard::take_from_bytes::<String>(rest).map_err(|error| error.to_string())?;
    rest = tail;
    let (cert, _) =
        postcard::take_from_bytes::<DeviceCert>(rest).map_err(|error| error.to_string())?;
    Ok(cert)
}

impl std::fmt::Debug for Workspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspaces")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl Workspaces {
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

    /// The content keys of `route`'s channel, for this host's own endpoints
    /// (the workspace actions it runs for its members' pages and bots), as the
    /// device it runs on answers: `create` opens the channel for the record the
    /// writes are about to start, and otherwise only an existing channel is
    /// read. `None` when this host runs on a device that takes part in no
    /// channel of the workspace.
    pub async fn own_route_keys(
        &self,
        route: &tilo_peer_state::document_frame::DocumentRoute,
        create: bool,
    ) -> ChannelKeysAnswer {
        let device = self.device_keys()?;
        let named = collab_types::ChannelRoute::new(route.app_id.as_str(), route.record.as_str());
        device.channel_keys(&route.cluster_id, &named, create).await
    }

    /// These workspaces, held without keeping them alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakWorkspaces {
        WeakWorkspaces(Arc::downgrade(&self.inner))
    }

    /// Open every workspace recorded under `dir` and bind each to the relay:
    /// its workspace pinned and admitting only governed members, its
    /// unpublished governance retained, its members' new records created.
    pub(crate) async fn open(
        host: WorkspaceServices,
        dir: PathBuf,
        store: PathBuf,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        // A workspace a host made keeps the certificate it made it with:
        // everything done for it compares that certificate exactly, so a host
        // whose own was renamed serves with the record's own (the same key
        // and validity, verified), rather than refusing the workspace.
        let host = host_serving(host, &dir);
        let listings = listing::Listings::open(&dir);
        let workspaces = Arc::new(device_plane::WorkspacesMap::default());
        let (plane, jobs) = device_plane::plane(Arc::clone(&workspaces), host.key.clone());
        let this = Self {
            inner: Arc::new(WorkspacesInner {
                store: Arc::new(
                    tilo_peer_state::store::Store::open(
                        store,
                        tilo_peer_state::store::Appetite::StoreAll,
                    )
                    .map_err(|error| error.to_string())?,
                ),
                direct: tilo_peer_state::direct::Serving::new(),
                dir: dir.clone(),
                plane,
                jobs,
                workspaces,
                calls: Arc::default(),
                creating: AsyncMutex::new(()),
                changes: tokio::sync::broadcast::channel(64).0,
                record_writes: tokio::sync::broadcast::channel(256).0,
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                unbuildable_windows: std::sync::Mutex::default(),
                listings,
                #[cfg(feature = "agents")]
                resealing: std::sync::atomic::AtomicBool::new(false),
                hosting: crate::hosted::Hosting::new(host.clone()),
                host,
                wait_list_mail: std::sync::OnceLock::new(),
                own: std::sync::OnceLock::new(),
                agent_door: agent_door::AgentDoor::default(),
            }),
        };
        let device = this.device_keys();
        let mut workspaces = BTreeMap::new();
        let entries = std::fs::read_dir(&dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_none_or(|extension| extension != "room") {
                continue;
            }
            // A workspace this host released stays on disk, unserved.
            if path.with_extension(RELEASED).exists() {
                continue;
            }
            let workspace =
                GovernedWorkspace::open(path.clone(), &this.inner.host.cert, device.as_ref())
                    .map_err(|error| format!("{}: record: {error}", path.display()))?;
            this.serve_direct(&workspace)?;
            workspaces.insert(
                workspace.workspace().to_owned(),
                Arc::new(AsyncMutex::new(workspace)),
            );
        }
        *this.inner.workspaces.lock().await = workspaces;
        let workspaces = this
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            this.bind(&workspace)
                .await
                .map_err(|e| format!("bind: {e}"))?;
            tracing::info!("bound");
            this.settle(&mut workspace)
                .await
                .map_err(|e| format!("settle: {e}"))?;
            tracing::info!("settled");
            this.name_own_host(&mut workspace)
                .await
                .map_err(|e| format!("hosts: {e}"))?;
            this.recall(&mut workspace)
                .await
                .map_err(|e| format!("recall: {e}"))?;
            tracing::info!("recalled");
            this.prepare_desktop_record(&mut workspace)
                .await
                .map_err(|e| format!("desktop record: {e}"))?;
            tracing::info!("desktop record ready");
            // What the relay's log revoked while the workspace was closed.
            this.carry_revocations(&mut workspace, &[]).await?;
        }
        this.follow_activity();
        this.follow_missing_surfaces();
        this.inner.hosting.resume().await;
        Ok(this)
    }

    /// The certificate this host serves its workspaces with: its own, or the
    /// one a workspace it opened was made with, when a build renamed the
    /// host's since (see [`host_serving`]).
    #[must_use]
    pub fn serving_certificate(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// Create a new governed workspace named `name`.
    ///
    /// # Errors
    ///
    /// Returns why the workspace could not be created, persisted or bound.
    pub async fn create(&self, name: &str) -> Result<WorkspaceSummary, String> {
        self.create_workspace(name, None).await
    }

    /// Create a governed workspace named `name`, under the identity `workspace` when
    /// given: a workspace moving here keeps the workspace it is. Refused when
    /// this host holds that workspace already.
    ///
    /// # Errors
    ///
    /// The name is not 1–80 characters, `workspace` is no UUID or held here
    /// already, or the workspace could not be written.
    pub async fn create_workspace(
        &self,
        name: &str,
        workspace: Option<&str>,
    ) -> Result<WorkspaceSummary, String> {
        let _creating = self.inner.creating.lock().await;
        if let Some(workspace) = workspace
            && self.inner.workspaces.lock().await.contains_key(workspace)
        {
            return Err("this host has that room already".into());
        }
        if let Some(workspace) = workspace
            && self
                .inner
                .dir
                .join(workspace)
                .with_extension(RELEASED)
                .exists()
        {
            return Err("this host released that room; its files stay here".into());
        }
        // A workspace this host serves for its governor elsewhere is that
        // governor's: governing it here as well would fork it.
        if let Some(workspace_id) =
            workspace.and_then(|workspace| tilo_relay_protocol::room_workspace_id(workspace).ok())
        {
            let bound = self
                .inner
                .host
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned();
            if let Some(bound) = bound
                && bound.lock().await.admission == crate::workspace_access::AdmissionMode::Governed
            {
                return Err("this host has that room already".into());
            }
        }
        let device = self.device_keys();
        let workspace = match workspace {
            Some(workspace) => GovernedWorkspace::create_in(
                &self.inner.dir,
                workspace,
                name,
                (&self.inner.host.cert, &self.inner.host.key),
                device.as_ref(),
                SystemTime::now(),
            )?,
            None => GovernedWorkspace::create(
                &self.inner.dir,
                name,
                &self.inner.host.cert,
                &self.inner.host.key,
                device.as_ref(),
                SystemTime::now(),
            )?,
        };
        self.bind(&workspace).await?;
        let summary = workspace.summary();
        let workspace = Arc::new(AsyncMutex::new(workspace));
        self.inner
            .workspaces
            .lock()
            .await
            .insert(summary.workspace.clone(), Arc::clone(&workspace));
        let mut governed = workspace.lock().await;
        self.settle(&mut governed).await?;
        self.name_own_host(&mut governed).await?;
        self.serve_direct(&governed)?;
        drop(governed);
        tracing::info!(workspace = %summary.workspace, workspace_id = summary.workspace_id, "governed room created");
        Ok(summary)
    }

    /// Stop governing and serving `workspace`: its workspace admits nobody from
    /// now on, which ends its sessions here, and the host leaves it closed
    /// when it opens again. Its files stay on disk. A device releases a
    /// workspace after moving it to another host, whose hints its members follow.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or the release could not be recorded
    /// or its access narrowed.
    pub async fn release(&self, workspace: &str) -> Result<(), String> {
        let governed = self
            .inner
            .workspaces
            .lock()
            .await
            .get(workspace)
            .cloned()
            .ok_or("this host does not govern that room")?;
        let governed = governed.lock().await;
        // Recorded first: a host stopped halfway keeps it released.
        let marker = governed.path.with_extension(RELEASED);
        let file = std::fs::File::create(&marker).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if let Some(dir) = marker.parent() {
            std::fs::File::open(dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|error| error.to_string())?;
        }
        self.inner.workspaces.lock().await.remove(workspace);
        self.inner.direct.release(governed.workspace_id());
        self.inner
            .host
            .access
            .replace(
                governed.workspace_id(),
                true,
                GovernedAccess {
                    member_devices: Vec::new(),
                    member_user_roots: Vec::new(),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        tracing::info!(%workspace, workspace_id = governed.workspace_id(), "governed room released");
        drop(governed);
        let _ = self.inner.changes.send(workspace.to_owned());
        Ok(())
    }

    /// Name `workspace` `name`, in its governance (what members show) and in
    /// this relay's record of it. Naming it what it is called changes
    /// nothing.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, the name is not 1–80 characters, or
    /// the change could not be committed.
    pub async fn rename(&self, workspace: &str, name: &str) -> Result<WorkspaceSummary, String> {
        let name = valid_name(name)?;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        if governed.record.name == name
            && governed.peer().governance().workspace_name().as_deref() == Some(name)
        {
            return Ok(governed.summary());
        }
        let now = SystemTime::now();
        let key = &self.inner.host.key;
        let mut candidate = governed.peer().clone();
        candidate
            .governance_mut()
            .author_workspace_name(name, key, now)?;
        let prior = std::mem::replace(&mut governed.record.name, name.to_owned());
        let collaboration = governed.collaboration().clone();
        if let Err(error) = governed.commit(candidate, key, now, |_, _| Ok(collaboration)) {
            governed.record.name = prior;
            return Err(error);
        }
        self.settle(&mut governed).await?;
        Ok(governed.summary())
    }

    /// Every workspace this relay governs.
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
            summaries.push(workspace.lock().await.summary());
        }
        summaries
    }

    /// The delegated agents on `workspace`'s roster, acting or not now.
    ///
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
            .governed_agents(SystemTime::now());
        agents.map_err(CollaborationError::new)
    }

    /// The device that governs every workspace here: this relay's own.
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// The governor answering the workspace-join exchange for these workspaces.
    #[must_use]
    pub fn join_service(&self) -> tilo_peer_state::join_answer::JoinAnswer<Self> {
        tilo_peer_state::join_answer::JoinAnswer(self.clone())
    }

    /// The plane the host answers its workspaces' member devices on, as its
    /// own device: what another device of a workspace asks it where no member
    /// session runs in between ([`device_plane`]).
    #[must_use]
    pub fn plane(&self) -> tilo_peer_state::actions::ActionPlane {
        self.inner.plane.clone()
    }

    /// The direct links of the workspaces governed here, as the host's own
    /// device: a member device of a workspace syncs with it here, and catches
    /// up from the history the host keeps.
    #[must_use]
    pub fn direct(&self) -> tilo_peer_state::direct::Serving {
        self.inner.direct.clone()
    }

    /// Serve `workspace`'s replica over the direct link, keeping its history
    /// at the host's store, and take in what its members send there.
    fn serve_direct(&self, workspace: &GovernedWorkspace) -> Result<(), String> {
        let link = tilo_peer_state::direct::DirectSync::over(
            workspace.peer.clone(),
            self.inner.host.key.clone(),
        )
        .with_store(Arc::clone(&self.inner.store));
        self.inner.direct.serve(link.clone())?;
        self.follow_link(workspace, &link);
        Ok(())
    }

    /// Take in what `workspace`'s link admits and answer what its members ask
    /// there: a frame the link admitted is what a member's write through a
    /// relay session was, so it is followed as one ([`Self::absorb`]), and each
    /// ask for content is answered with what this host keeps of it, or word
    /// that it keeps none, addressed to whoever asked. The link carries each
    /// ask on to the workspace's other members itself.
    fn follow_link(
        &self,
        workspace: &GovernedWorkspace,
        link: &tilo_peer_state::direct::DirectSync,
    ) {
        let name = workspace.workspace().to_owned();
        let workspaces = WeakWorkspaces(Arc::downgrade(&self.inner));
        let link = link.clone();
        let mut heard = link.heard();
        tokio::spawn(async move {
            while let Ok(frame) = heard.recv().await {
                // The host this link served is gone: so is the answer.
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                let asking = matches!(
                    frame.message,
                    neighbour_wire::NeighbourMsg::RecordContentWant { .. }
                );
                let answering = matches!(
                    frame.message,
                    neighbour_wire::NeighbourMsg::RecordContentPiece { .. }
                        | neighbour_wire::NeighbourMsg::RecordContentLacks { .. }
                );
                let Ok(body) = postcard::to_allocvec(&frame.message) else {
                    continue;
                };
                if let Err(error) = workspaces.absorb(&name, frame.author, &body).await {
                    tracing::warn!(%name, %error, "the host did not keep what its link admitted");
                }
                // What a member answers the host with answers a request the
                // host made, and is the host's to keep: a device that keeps
                // its workspace's files reads the pieces here
                // ([`Self::content_for_host`]).
                if answering {
                    let (workspaces, name, author) =
                        (workspaces.clone(), name.clone(), frame.author);
                    let body = body.clone();
                    tokio::task::spawn_blocking(move || {
                        workspaces.content_for_host(&name, author, &body);
                    });
                    continue;
                }
                if !asking {
                    continue;
                }
                for answer in workspaces.content_answer(&name, &body) {
                    match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&answer) {
                        Ok(message) => {
                            let _ = link.send(message);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "a content answer did not encode");
                        }
                    }
                }
            }
        });
    }

    /// Run the transform an offloadable action declares for the workspaces
    /// governed here as a compute job on `jobs`: the host then answers the
    /// offers of its workspaces' devices with what `jobs` knows of itself, and
    /// runs the invocations it won through it ([`HostJobs`]).
    pub fn run_jobs(&self, jobs: Arc<dyn HostJobs>) {
        self.inner.jobs.serve(jobs);
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
        let now = SystemTime::now();
        let member = governed.peer().permissions(device, now).ok()?;
        drop(governed);
        member
            .allows(
                &member.policy.node(),
                tilo_action_policy::ActionKind::Undoable,
                now,
            )
            .then_some(member.role)
    }

    /// Workspace UUIDs, each sent after a governance decision on that workspace was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// The invitations members ask to mail to addresses on a wait list,
    /// for whoever runs the relay to send from the web entry `door`: until
    /// this is called, the workspaces send no mail and refuse to, and they mail
    /// only invitations on that door
    /// ([`tilo_relay_protocol::workspaces::invitation_on_door`]). `None` once
    /// called before.
    #[must_use]
    pub fn mail_wait_list(
        &self,
        door: &str,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<WaitListMail>> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.inner
            .wait_list_mail
            .set((door.to_owned(), sender))
            .ok()?;
        Some(receiver)
    }

    /// Each member write to a record the governor's copy took in. A slow
    /// receiver may lag; it then re-reads the records it follows.
    #[must_use]
    pub fn record_writes(&self) -> tokio::sync::broadcast::Receiver<RecordWrite> {
        self.inner.record_writes.subscribe()
    }

    /// The governor's copy of record `record` (`""` for the primary) of
    /// `app` in `workspace`, saved: a record members write sealed is read with
    /// the keys this relay holds as a member of its channel.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here, or a sealed record cannot be opened.
    pub async fn record_document(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        #[cfg(any(feature = "agents", feature = "web"))]
        let document = governed
            .readable_document(app, record)
            .await
            .map_err(CollaborationError::new)?;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let document = governed.peer().document(app, record);
        drop(governed);
        Ok(document)
    }

    /// The `(app, record)` of every record of `workspace` the governor's copy
    /// holds readable: in the clear, or sealed under channels this relay
    /// takes part in ([`Self::record_document`] opens those).
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn record_routes(
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
        Ok(self.workspace(workspace).await?.lock().await.summary())
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

    /// Every person governance records in `workspace` other than its owner, by
    /// person (hex root), with the workspace's governance generation.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn membership(
        &self,
        workspace: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = SystemTime::now();
        let state = self.workspace(workspace).await?;
        let governed = state.lock().await;
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let owner = self.inner.host.cert.user_public_key;
        let marks = governed
            .peer()
            .governance()
            .governed_members(now)
            .unwrap_or_default()
            .into_iter()
            .filter(|member| member.user_public_key != owner)
            .map(|member| {
                let person = hex::encode(member.user_public_key);
                let role = if member.administrator {
                    "owner"
                } else {
                    match member.role {
                        tilo_peer_state::join::MemberRole::Viewer => "viewer",
                        tilo_peer_state::join::MemberRole::Commenter => "commenter",
                        tilo_peer_state::join::MemberRole::Editor => "editor",
                        tilo_peer_state::join::MemberRole::Moderator => "moderator",
                    }
                };
                let mark = MemberMark {
                    role: role.to_owned(),
                    removed: member.removed,
                    banned: policy.is_blocked(&person),
                };
                (person, mark)
            })
            .collect();
        Ok((governed.summary().generation, marks))
    }

    /// The apps `workspace` lets author its content
    /// ([`tilo_action_policy::roles::app_authorship`]), as its governance
    /// holds them now.
    ///
    /// # Errors
    ///
    /// The workspace is not governed here.
    pub async fn app_authors(
        &self,
        workspace: &str,
    ) -> Result<std::collections::BTreeSet<String>, CollaborationError> {
        let now = SystemTime::now();
        let state = self.workspace(workspace).await?;
        let policy = state
            .lock()
            .await
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)?
            .policy;
        let cx = tilo_action_policy::Context::at_time(now);
        Ok(policy
            .grants
            .iter()
            .filter_map(|grant| match &grant.subject {
                tilo_action_policy::Subject::App(app) => Some(app.clone()),
                _ => None,
            })
            .filter(|app| policy.app_authors(app, &cx))
            .collect())
    }

    /// The workspace-join contract's membership answer for `device`, a
    /// device the workspace just admitted to subscribe ([`member_bootstrap`]).
    pub(crate) async fn subscriber_bootstrap(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Option<Vec<u8>> {
        let workspace = self.workspace(workspace).await.ok()?;
        let workspace = workspace.lock().await;
        member_bootstrap(
            &workspace.peer(),
            &workspace.record.authority,
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
            .ok_or_else(|| CollaborationError::new("this relay does not govern that room"))
    }

    /// Pin the workspace, admitting only its governed members, and
    /// follow its members' writes: create the records they start and keep the
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
            true,
            crate::workspace_access::AdmissionMode::Governed,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = runtime_state.lock().await;
        // This relay's workspace authority governs the workspace: never auto-enrol,
        // even if the workspace was autobound before the authority took it.
        if std::mem::replace(
            &mut runtime.admission,
            crate::workspace_access::AdmissionMode::Governed,
        ) != crate::workspace_access::AdmissionMode::Governed
        {
            crate::authority_epoch::bump();
        }
        runtime.workspace_authority = Some(WorkspaceAuthority {
            workspaces: Arc::downgrade(&self.inner),
            workspace: workspace.clone(),
        });
        if runtime.governor_tap.is_none() {
            let (tap, notices) = tokio::sync::mpsc::channel(RECORD_NOTICES);
            runtime.governor_tap = Some(tap);
            self.follow(workspace, notices);
        }
        Ok(())
    }

    /// Follow each record write a member makes, see [`Self::absorb`], and
    /// replay what the relay retains each time an app surface arrives: the
    /// writes held for it, those let go past the held queue's bound
    /// included, are taken in then rather than when the next write lands.
    fn follow(
        &self,
        workspace: String,
        mut notices: tokio::sync::mpsc::Receiver<([u8; 32], Vec<u8>)>,
    ) {
        let workspaces = Arc::downgrade(&self.inner);
        let mut arrivals = crate::surfaces::arrivals();
        arrivals.mark_unchanged();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    notice = notices.recv() => {
                        let Some((author, body)) = notice else { break };
                        let Some(inner) = workspaces.upgrade() else { break };
                        if let Err(error) = (Self { inner }).absorb(&workspace, author, &body).await {
                            tracing::warn!(%workspace, %error, "governed room did not follow a record write");
                        }
                    }
                    Ok(()) = arrivals.changed() => {
                        let Some(inner) = workspaces.upgrade() else { break };
                        (Self { inner }).replay_soon(workspace.clone());
                    }
                }
            }
        });
    }

    /// Replay what the relay retains for `workspace` into the governor's copy
    /// ([`Self::recall_retained`]), apart from whatever asked for it. The
    /// relay's copy is read once every write the governor was handed is
    /// durable in it, and before the workspace is locked: their delivery waits
    /// for the governor to follow them.
    fn replay_soon(&self, workspace: String) {
        let workspaces = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let Some(inner) = workspaces.upgrade() else {
                return;
            };
            let workspaces = Self { inner };
            let Ok(state) = workspaces.workspace(&workspace).await else {
                return;
            };
            let workspace_id = state.lock().await.workspace_id();
            let retained = match workspaces
                .inner
                .host
                .records
                .retained_settled(workspace_id)
                .await
            {
                Ok(retained) => retained,
                Err(error) => {
                    tracing::warn!(%workspace, %error, "governed room did not replay what its members wrote");
                    return;
                }
            };
            #[cfg(any(feature = "agents", feature = "web"))]
            workspaces
                .found_retained(&workspace, &state, &retained)
                .await;
            workspaces.recall_retained(&mut *state.lock().await, retained);
        });
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
        retained: &[crate::workspace_records::RetainedBody],
    ) {
        let mut first: std::collections::BTreeMap<
            (String, String),
            &crate::workspace_records::RetainedBody,
        > = std::collections::BTreeMap::new();
        for retained in retained {
            // What the relay's own device wrote the governor holds already, or
            // took in as it came ([`Self::recall_retained`]).
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            let Ok(neighbour_wire::NeighbourMsg::AppDocument {
                app_id,
                record_id,
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
            first.entry((app_id, record_id)).or_insert(retained);
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
                .absorb(workspace, retained.author, &retained.body)
                .await
            {
                tracing::warn!(%workspace, %error, "governed room did not found a record its members wrote");
            }
        }
    }

    /// Ask the participants of `app`'s `record`'s channel in `workspace` to
    /// admit this host's device ([`Self::join_channel`]), apart from whatever
    /// asked for it: the group's answer arrives as another write of the
    /// workspace, which waits for the writes ahead of it, a notice in this
    /// one's queue included. Once it is admitted, what the relay retains of
    /// the record is replayed ([`Self::replay_soon`]), so its copy starts
    /// from its first write. Asked once per record until the first answer.
    #[cfg(any(feature = "agents", feature = "web"))]
    fn join_soon(&self, workspace: &mut GovernedWorkspace, app: String, record: String) {
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
            let joined = workspaces.join_channel(&name, &app, &record).await;
            let Ok(workspace) = workspaces.workspace(&name).await else {
                return;
            };
            workspace.lock().await.asking.remove(&(app, record));
            if joined {
                workspaces.replay_soon(name);
            }
        });
    }

    /// Ask to be let into the channel that seals `record` of `app` in
    /// `workspace`, when this host takes part in the workspace's channels:
    /// what a host that reads a record its members write sealed asks for once
    /// it finds the record shut, since its keys arrive only after the
    /// channel's participants admit it ([`Self::join_soon`]). Nothing when
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

    /// Take in `message` if it is a notice rather than a record write: an
    /// access request, a channel commit, the revocations an identity hello
    /// carries, or the workspace's topology. Hands a record write back.
    async fn absorb_notice(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<Option<neighbour_wire::NeighbourMsg>, String> {
        if channel_workspace(&message).is_some() {
            self.take_channel_governance(workspace, author, message)
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
                room: ref named, ..
            } if named == workspace => self.absorb_topology(workspace, author, message).await,
            _ => return Ok(Some(message)),
        }
        .map(|()| None)
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
    async fn absorb(&self, workspace: &str, author: [u8; 32], body: &[u8]) -> Result<(), String> {
        let host = &self.inner.host;
        // Every member's record write reaches the governor here, so this is
        // where one that does not decode is reported (the content classifier
        // that reads the same body before it leaves it to this).
        let message = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
            .map_err(|error| format!("a member's write does not decode: {error}"))?;
        // The relay's own device writes records through the workspace too: the
        // actions it runs for members (a visitor linking their Discord
        // account) are committed as that device, and the governor's copy,
        // which the workspace's bots read, must take them in like any member's.
        // Its notices it made itself, so it has them already. A device
        // hosting its own workspaces is also their member under the same key, and
        // that member's windows (its topology) are its own to send: those
        // are taken in like any member's.
        // A host told of its device's channel service retains its own
        // device's channel governance like any member's, for the other
        // participants: the host keeps no channels to have it already.
        if author == host.cert.device_public_key
            && !matches!(
                message,
                neighbour_wire::NeighbourMsg::AppDocument { .. }
                    | neighbour_wire::NeighbourMsg::SyncResp { .. }
                    | neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
                    | neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest { .. }
                    | neighbour_wire::NeighbourMsg::MlsGovernance { .. }
            )
        {
            return Ok(());
        }
        let Some(message) = self.absorb_notice(workspace, author, message).await? else {
            return Ok(());
        };
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } = &message
        else {
            return Ok(());
        };
        let (app_id, record_id) = (app_id.clone(), record_id.clone());
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
        crate::timing::window("room.follow_wait", waiting);
        if workspace.peer().holds_document(&app_id, &record_id) {
            self.follow_write(&mut workspace, author, message, (app_id, record_id))
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
        let readable = !sealed || workspace.takes_part_in_channel(&app_id, &record_id).await;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let readable = !sealed;
        if !readable {
            #[cfg(any(feature = "agents", feature = "web"))]
            self.join_soon(&mut workspace, app_id, record_id);
            return Ok(());
        }
        let now = SystemTime::now();
        let member = workspace
            .peer()
            .permissions(author, now)
            .map_err(|error| error.to_string())?;
        let node =
            collab_types::validator::document_node(&member.policy.workspace, &app_id, &record_id);
        if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, now) {
            return Err(format!(
                "{app_id} record written by a device that may not edit"
            ));
        }
        // A record starts from the write that founds it: a write that is the
        // whole record founds the route on its own, and is not preceded by a
        // cleartext genesis the members, who read only what the channel seals,
        // would have to account for. The checkpoint includes the write that
        // created a record founded here in the clear: the relay retains a
        // member's changes only as a tail of a checkpoint that resolves their
        // dependencies.
        let mut candidate = workspace.peer().clone();
        if !envelope.is_whole_record() {
            candidate.publication_mut().publish_document(
                &app_id,
                &record_id,
                automerge::AutoCommit::new().save(),
                &host.key,
                now,
            )?;
        }
        candidate.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            now,
        )?;
        // A sealed write stays in the record's sealed history; a readable
        // one is folded into the governor's checkpoint.
        if !sealed {
            let written = candidate
                .document(&app_id, &record_id)
                .ok_or("created record missing")?;
            candidate
                .publication_mut()
                .publish_document(&app_id, &record_id, written, &host.key, now)?;
        }
        workspace.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut workspace).await
    }

    /// Take a member's own windows into the workspace's governance: the changes
    /// a member publishes to the workspace's arrangement. The workspace's admission decides
    /// them exactly as every member's does (a change to what someone else
    /// made, without the workspace's leave, is refused); the governor commits what
    /// it admitted as the workspace's next generation, which every member then
    /// takes in. Until this, a window someone opened after joining reached
    /// nobody: members take governance only from the governor, and the
    /// governor never took these in, so the member published them again
    /// every few seconds for nothing.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    async fn absorb_topology(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut workspace = workspace.lock().await;
        if let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = &message {
            let missing = unbuildable_windows::missing_history(
                workspace.peer().admitted_workspace(),
                changes,
            )?;
            if missing > 0 {
                return Err(self.unbuildable_windows(workspace.workspace(), missing));
            }
        }
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        let outcome = candidate.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            now,
        )?;
        if !outcome.changed {
            return Ok(());
        }
        // A member removed takes every way in the workspace handed them with
        // them: an invitation issued to them before, still unredeemed,
        // would otherwise admit them again at once.
        let removed: std::collections::BTreeSet<_> = member_roots(&workspace.peer(), now)
            .difference(&member_roots(&candidate, now))
            .copied()
            .collect();
        // The person `author` writes for, when `author` is one of them: who
        // left the workspace themselves is not kept out of it.
        let author_person = workspace
            .summary()
            .members
            .iter()
            .find(|member| member.active_device_keys.contains(&author))
            .map(|member| member.user_public_key);
        workspace.commit(candidate, &host.key, now, |record, _| {
            let mut state = record.collaboration.clone();
            state.withdraw_invitations(&removed);
            Ok(state)
        })?;
        workspace.keep_out_removed(&removed, author_person, tilo_clock::unix_now_ns())?;
        self.settle(&mut workspace).await
    }

    /// Queue `message`, a member's channel governance, for `workspace`'s
    /// channel service. True when it is the first since they were last handed
    /// over, so its sender must start the hand-over.
    fn queue_channel_notice(
        &self,
        workspace: &str,
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<bool, String> {
        let mut queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        let waiting = queued.entry(workspace.to_owned()).or_default();
        let first = waiting.len() < MAX_QUEUED_CHANNEL_NOTICES && {
            waiting.push(message);
            waiting.len() == 1
        };
        drop(queued);
        Ok(first)
    }

    /// Take in `author`'s access request or commit for a channel of `workspace`.
    ///
    /// This host holds no channel keys of its own. One whose device's channel
    /// service it runs queues what members send for that service, which settles
    /// the groups; one that is no device's peer keeps what the channels are
    /// made of like any other governance, for whoever takes part; and one
    /// beside a channel service running on the device itself keeps nothing,
    /// since that service hears these on its own sessions.
    async fn take_channel_governance(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        if channel_workspace(&message).is_none_or(|named| named != workspace) {
            return Ok(());
        }
        let Some(channels) = &self.inner.host.channels else {
            // What a governed member device sent, and only that.
            let state = self
                .workspace(workspace)
                .await
                .map_err(|error| error.to_string())?;
            let (workspace_id, member) = {
                let governed = state.lock().await;
                (
                    governed.workspace_id(),
                    governed
                        .peer()
                        .governance()
                        .governed_access(SystemTime::now())
                        .is_ok_and(|access| access.member_devices.contains(&author)),
                )
            };
            if !member {
                return Ok(());
            }
            return self
                .retain_channel_governance(workspace_id, vec![message])
                .await;
        };
        if channels.sink.is_some() {
            self.channel_notice(workspace, message)?;
        }
        Ok(())
    }

    /// Start handing `workspace`'s queued channel governance over. A crowd
    /// joining sends a request each, so notices only queue here; the first
    /// since they were last taken starts a task that hands all of them over
    /// at once.
    fn channel_notice(
        &self,
        workspace: &str,
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<(), String> {
        if self.queue_channel_notice(workspace, message)? {
            let workspaces = self.clone();
            let workspace = workspace.to_owned();
            tokio::spawn(async move {
                if let Err(error) = workspaces.take_channel_notices(&workspace).await {
                    tracing::warn!(%workspace, %error, "the room's channels did not take their notices");
                }
            });
        }
        Ok(())
    }

    /// Hand `workspace`'s channel service every queued notice: the workspace's
    /// own governance is retained before the channel governance a service
    /// built on it decides them with.
    async fn take_channel_notices(&self, workspace: &str) -> Result<(), String> {
        let sink = self
            .inner
            .host
            .channels
            .as_ref()
            .and_then(|channels| channels.sink.clone());
        let Some(sink) = sink else {
            self.inner
                .channel_notices
                .lock()
                .map_err(|_| "channel notices poisoned")?
                .remove(workspace);
            return Ok(());
        };
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let publisher = {
            let governed = governed.lock().await;
            Arc::clone(&governed.publisher)
        };
        // Whatever settle handed over is retained before the channel
        // governance a service derives from it.
        publisher.flushed().await;
        // Taken under the service: notices that queue while it works start the
        // next hand-over, which takes all of them at once.
        let waiting = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?
            .remove(workspace)
            .unwrap_or_default();
        sink.take(waiting);
        Ok(())
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
    pub async fn refound_record(
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
        let now = SystemTime::now();
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
            let keys = workspace.record_keys(app, record).await;
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
        sealed_records::refound(
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
        let now = SystemTime::now();
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
    /// the record), tell whoever follows the workspace's records, and file its
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
        crate::timing::window("room.follow", following);
        #[cfg(any(feature = "agents", feature = "web"))]
        {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(workspace).await {
                tracing::warn!(workspace = %workspace.workspace(), %error, "governed room did not compact a sealed record");
            }
            crate::timing::window("room.compact", compacting);
        }
        let _ = self.inner.record_writes.send(RecordWrite {
            workspace: workspace.workspace().to_owned(),
            app,
            record,
        });
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
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        let filed = candidate
            .workspace_mut()
            .refuse_write(&host.cert, message, reason, &host.key, now)?;
        if !filed {
            return Ok(());
        }
        workspace.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
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
        if !matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
            return Ok(());
        }
        let state = self
            .workspace(workspace)
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
            SystemTime::now(),
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

    /// Write the members' writes `workspace` took in through to its workspace file
    /// within [`KEEP_FOLLOWED`], once for all of them: a write each made a
    /// busy chat queue behind its own workspace file.
    fn keep_soon(&self, workspace: &mut GovernedWorkspace) {
        workspace.unkept = true;
        if std::mem::replace(&mut workspace.keeping, true) {
            return;
        }
        let workspaces = Arc::downgrade(&self.inner);
        let id = workspace.workspace().to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(KEEP_FOLLOWED).await;
            let Some(inner) = workspaces.upgrade() else {
                return;
            };
            let Ok(workspace) = (Self { inner }).workspace(&id).await else {
                return;
            };
            let mut workspace = workspace.lock().await;
            workspace.keeping = false;
            if workspace.unkept {
                workspace.keep_followed();
            }
        });
    }

    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<Vec<crate::workspace_records::WorkspaceAdmission>, String> {
        let mut admissions = Vec::with_capacity(messages.len());
        for message in messages {
            let commit = channel_commit(&message)?;
            admissions.push(
                self.inner
                    .host
                    .records
                    .enqueue(workspace_id, commit)
                    .await?,
            );
        }
        Ok(admissions)
    }

    /// Replay every record write the relay retains for `workspace` into the
    /// governor's copy, and keep what it took in.
    async fn recall(&self, workspace: &mut GovernedWorkspace) -> Result<(), String> {
        let retained = self
            .inner
            .host
            .records
            .retained(workspace.workspace_id())
            .await?;
        self.recall_retained(workspace, retained);
        Ok(())
    }

    /// Take `retained`, the record writes the relay retains for `workspace`, into
    /// the governor's copy, and keep what it took in. A write held for an
    /// app surface not here yet may be let go past the held queue's bound,
    /// and so is every later write of its author to its record, which
    /// would otherwise overtake it for good. The relay still retains them
    /// all: they are replayed again, in the order their authors wrote them,
    /// once the surface arrives ([`Self::follow`]) or something they waited
    /// for lands ([`Followed::replay_wanted`]).
    fn recall_retained(
        &self,
        workspace: &mut GovernedWorkspace,
        retained: Vec<crate::workspace_records::RetainedBody>,
    ) {
        let (mut changed, mut again) = (false, false);
        for retained in retained {
            // What the relay's device wrote is retained under its key, the
            // governor's own commits and its sessions' writes alike: the
            // first are the governor's copy already, the second it took in
            // as they came (`absorb`). Neither is replayed into it.
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            let message: neighbour_wire::NeighbourMsg = match postcard::from_bytes(&retained.body) {
                Ok(message) => message,
                Err(error) => {
                    tracing::warn!(
                        author = %hex::encode(retained.author),
                        %error,
                        "a retained write does not decode; it is not replayed"
                    );
                    continue;
                }
            };
            let followed = workspace.follow(retained.author, message.clone());
            let Followed {
                changed: took,
                replay_wanted,
                refused,
            } = followed;
            changed |= took;
            again |= replay_wanted;
            if let Some(reason) = refused {
                let filed = workspace.refused(
                    &self.inner.host.cert,
                    &self.inner.host.key,
                    &message,
                    &reason,
                );
                changed |= filed.changed;
            } else if took && self.landed(workspace, &message) {
                changed = true;
            }
        }
        if changed {
            workspace.keep_followed();
        }
        // What a write let go waited for landed in this replay: the relay's
        // copy is read again, with what reached it since this one was read.
        // Each further replay needs something new to land, so it ends.
        if again {
            self.replay_soon(workspace.workspace().to_owned());
        }
    }

    /// Bring the relay in line with the workspace's committed state: admit exactly
    /// its governed members, then retain every governance change not yet
    /// published, then re-derive its calls' participants. Runs after each
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
            tracing::debug!(workspace = %workspace.workspace(), "room settled without the devices whose certificates expired");
        }
        let now = SystemTime::now();
        let access: GovernedAccess = workspace.peer().governance().governed_access(now)?;
        host.access
            .replace(workspace.workspace_id(), true, access)
            .await
            .map_err(|error| error.to_string())?;
        let standings = workspace
            .peer()
            .governance()
            .governed_standings(SystemTime::now())?;
        host.access
            .replace_standings(workspace.workspace_id(), true, standings)
            .await
            .map_err(|error| error.to_string())?;
        let publisher = Arc::clone(&workspace.publisher);
        if workspace.rebased {
            publisher.flushed().await;
            self.archive_room_history(workspace).await?;
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
        self.observe_calls(workspace);
        self.governance_moved(workspace);
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
    }

    /// Archive what the relay retains of `workspace`'s document, once every
    /// governance change published before is retained: the workspace starts over
    /// from the base its next publication carries.
    async fn archive_room_history(&self, workspace: &GovernedWorkspace) -> Result<(), String> {
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

    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `workspace`.
    fn transport_hint(
        &self,
        workspace: &GovernedWorkspace,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let host = &self.inner.host;
        let endpoint_addr = host.endpoint.addr();
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: host.mode,
            room: workspace.workspace().to_owned(),
            workspace_id: workspace.workspace_id(),
            endpoint_addr,
            host_user_public_key: host.cert.user_public_key,
            host_device_certificate: host.cert.clone(),
            packet_relay_url,
        }
        .transport_hint(&host.key)
        .map_err(|error| CollaborationError::new(error.to_string()))
    }

    fn revocations(&self) -> RevocationList {
        self.inner.host.revocations.read_recover().clone()
    }
}

/// The workspace a channel governance message names.
fn channel_workspace(message: &neighbour_wire::NeighbourMsg) -> Option<&str> {
    match message {
        neighbour_wire::NeighbourMsg::MlsGovernance { workspace, .. }
        | neighbour_wire::NeighbourMsg::MlsAccessRequest { workspace, .. } => Some(workspace),
        neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest { room, .. } => Some(room),
        _ => None,
    }
}

/// A channel governance message, retained as this relay's own.
fn channel_commit(
    message: &neighbour_wire::NeighbourMsg,
) -> Result<crate::workspace_records::HostCommit, String> {
    let (channel, retention) = neighbour_wire::retention(message, false)
        .ok_or("channel governance has no retention channel")?;
    let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    Ok(crate::workspace_records::HostCommit {
        body,
        channel,
        checkpoint: retention == neighbour_wire::Retention::Checkpoint,
    })
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// The workspace-join contract's membership answer for `device`, a device
/// the workspace `peer` replicates just admitted to subscribe, postcard encoded:
/// what it builds its replica of the workspace from before the workspace's replay
/// ([`tilo_relay_protocol::Envelope::WorkspaceBootstrap`]). A device of the
/// workspace's owner gets the owner's answer, any other member's device its
/// role's. The governance messages and
/// records stay out: the replay that follows carries them.
pub(crate) fn member_bootstrap(
    peer: &StatePeer,
    host: &DeviceCert,
    workspace_id: u64,
    generation: u64,
    device: &DeviceCert,
) -> Option<Vec<u8>> {
    use tilo_peer_state::join::{
        JoinBootstrap, JoinTransportResponse, MemberRole, OwnerBootstrapV1,
    };
    let role = peer
        .permissions(device.device_public_key, SystemTime::now())
        .map_or(collab_types::Role::Viewer, |authority| authority.role);
    let answer = match role {
        collab_types::Role::Owner => {
            JoinTransportResponse::OwnerMembershipComplete(OwnerBootstrapV1 {
                version: 1,
                device_cert: device.clone(),
                room: peer.workspace_id().to_owned(),
                workspace_id,
                host: host.clone(),
                certified_roster: peer.certificates(),
                revocations: peer.revocations(),
                records: Vec::new(),
                governance_messages: Vec::new(),
                generation,
            })
        }
        role => JoinTransportResponse::MembershipComplete(JoinBootstrap {
            version: 1,
            invite_id: [0; 32],
            device_cert: device.clone(),
            role: match role {
                collab_types::Role::Commenter => MemberRole::Commenter,
                collab_types::Role::Editor => MemberRole::Editor,
                collab_types::Role::Moderator => MemberRole::Moderator,
                _ => MemberRole::Viewer,
            },
            room: peer.workspace_id().to_owned(),
            workspace_id,
            host: host.clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: Vec::new(),
            governance_messages: Vec::new(),
            generation,
        }),
    };
    postcard::to_stdvec(&answer).ok()
}
