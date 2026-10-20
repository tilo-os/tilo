//! Rooms this relay governs.
//!
//! A shared workspace needs one authority that decides who is in it: it
//! issues invitations, admits the device that redeems one, changes roles and
//! removes members, and publishes each decision as signed room governance
//! every member verifies. Here that authority is the relay hosting the room.
//! Its own device is the room's governor: it keeps the room's [`StatePeer`],
//! publishes governance as its own retained changes of the room's workspace,
//! and hands the governed member set to the relay's admission after every
//! decision, so a member it admits can subscribe at once and a member it
//! removes is cut off.
//!
//! Rooms are administered as their own `tilo.workspace` governance actions,
//! and the host's own operations (creating, listing and entering rooms) as
//! `tilo.host` actions (`actions`), or, from a page, over HTTPS: by devices
//! of the relay's own user, and by other members as far as the room's
//! governance lets them (`authority`, one decision for every door).
//! [`ROOM_ADMIN_ALPN`] still removes members, until the actions can ask the
//! person's own confirmation for it. Any device redeems an invitation over the shared workspace-join
//! protocol, and a page over HTTPS, where someone the room lets invite then
//! admits it.

mod actions;
mod admin;
mod admissions;
#[cfg(feature = "agents")]
mod agent_members;
#[cfg(feature = "agents")]
mod applications;
mod authority;
mod collaboration;
mod content;
pub use content::RoomContent;
mod desktop_writes;
pub(crate) use desktop_writes::writes_desktop;
mod listing;
mod member_records;
pub use member_records::PreparedRecord;
mod pages;
mod peer_parts;
mod private;
#[cfg(test)]
mod private_network_tests;
mod private_store;
pub use private::PrivateProtocol;
mod publisher;
mod revocations;
#[cfg(any(feature = "agents", feature = "web"))]
pub(crate) mod sealed_records;
#[cfg(feature = "web")]
mod spectator_keys;
#[cfg(feature = "agents")]
mod steward_endpoint;
mod stewardship;
#[cfg(test)]
pub(crate) mod tests;
mod topology_namespace;
#[cfg(feature = "agents")]
mod views;
mod voice;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use serde::{Deserialize, Serialize};
use tilo_peer_state::{GovernedAccess, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

pub(crate) use actions::{GovernedPlane, HostRoomActions};
pub use admin::RoomAdminProtocol;
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{CollaborationError, GovernanceCompletion, RoomJoinService};
pub use pages::PreAdmitRequest;
// The administration vocabulary (the actions' names and answers, and the
// wire door's) and the device's client, under the daemon's historic paths.
pub use tilo_relay_session::rooms::{
    ActionInvitation, CLAIM_TOPOLOGY_NAMESPACE, CREATE_ROOM, CancelInvitationRequest, CreatedRoom,
    ENTER_ROOM, EnteredRoom, GRANT_APP_AUTHORSHIP, GrantAppAuthorshipRequest,
    HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, IssueInvitationRequest, IssuedInvitation,
    LIST_ROOMS, MEMBER_ACTIONS_PROVIDER, MEMBER_RESOURCE_KIND, PREPARE_APPLICATION_RECORD,
    PendingInvitation, RECORD_EFFECT_RECEIPT, ROOM_ADMIN_ALPN, RemoveMemberRequest,
    RoomAdminClient, RoomAdminError, RoomCommand, RoomListing, RoomReply, RoomSummary,
    SetMemberRoleRequest, member_resource,
};

use crate::peer::WorkspaceRegistry;
use tilo_lock::RwLockRecover as _;

const RECORD_VERSION: u32 = 2;
/// Governance publications kept after the last full checkpoint before the
/// room's publication is compacted into a fresh one.
const MAX_PUBLICATION_TAIL: usize = 256;
/// Record writes the room authority has yet to look at, per room.
const RECORD_NOTICES: usize = 256;

/// One governance message this room has published, in publication order.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Publication {
    bytes: Vec<u8>,
    checkpoint: bool,
}

/// Everything a governed room keeps across restarts. Written atomically
/// after each decision, before it is published or answered; the large parts
/// of `peer` are files of their own ([`peer_parts`]), written before it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RoomRecord {
    version: u32,
    room: String,
    workspace_id: u64,
    name: String,
    authority: DeviceCert,
    peer: Vec<peer_parts::PeerPart>,
    generation: u64,
    /// The complete committed publication: the last full checkpoint and every
    /// change after it. A joining member's bootstrap carries all of it.
    outgoing: Vec<Publication>,
    /// How much of `outgoing` the relay has retained, as of the room's last
    /// write. Publishing does not write the room again: after a crash the
    /// room republishes what it published since, which members tolerate.
    published: usize,
    collaboration: collaboration::CollaborationState,
}

/// The devices governance lets read a room's channels: the room's governed
/// member devices (`access`, as `peer` derived it at `now`), each record's
/// channel less the devices a Read Deny on that record, its app or a pattern
/// over it withholds. The record of a channel is named by a grant or by one
/// of the routes `steward` keeps; a channel whose record is neither is
/// withheld from whoever a Deny over a pattern could keep out of it.
fn readers(
    peer: &StatePeer,
    access: &GovernedAccess,
    steward: Option<&stewardship::Steward>,
    now: SystemTime,
) -> Result<stewardship::Readers, String> {
    let key = |device: [u8; 32]| collab_types::Ed25519PublicKey::from_bytes(device);
    let routes = steward
        .map(stewardship::Steward::routes)
        .unwrap_or_default();
    let record_readers = peer.governance().withheld_record_readers(now, &routes)?;
    let withheld = record_readers
        .withheld
        .into_iter()
        .map(|(channel, devices)| (channel, devices.into_iter().map(key).collect()))
        .collect();
    Ok(stewardship::Readers::new(
        access.member_devices.iter().copied().map(key).collect(),
        withheld,
        record_readers.unrouted.into_iter().map(key).collect(),
    ))
}

/// The stewards of `room`'s channels, under `dir`, when `authority` (this
/// relay) may own workspaces.
fn stewardship(
    dir: &Path,
    room: &str,
    authority: &DeviceCert,
    key: &SigningKey,
) -> Option<Arc<stewardship::Steward>> {
    authority.capabilities.can_own_workspaces.then(|| {
        Arc::new(stewardship::Steward::new(stewardship::Stewardship::new(
            room,
            dir.join(format!("{room}.channels")),
            key,
        )))
    })
}

/// A room this relay governs.
struct GovernedRoom {
    record: RoomRecord,
    peer: StatePeer,
    path: PathBuf,
    /// The files of `record.peer`'s large parts.
    parts: peer_parts::PeerParts,
    /// Its channels' stewards, when this relay is a node that may own
    /// workspaces ([`stewardship`]). Locked apart from the room: admitting
    /// a crowd's devices to the channels held the room for an MLS change
    /// and its writes per batch, and every join queued behind them. Taken
    /// after the room by whoever holds both.
    stewardship: Option<Arc<stewardship::Steward>>,
    /// The owner's pre-admissions and notices.
    admissions: admissions::Admissions,
    /// Admits the governance each settle retains to the host's order, after
    /// the room's lock is released.
    publisher: Arc<publisher::RoomPublisher>,
    /// Member writes to follow before compacting a sealed record history
    /// again, after a compaction failed: each attempt opens and merges the
    /// whole history.
    #[cfg(any(feature = "agents", feature = "web"))]
    compaction_wait: usize,
    /// The room's shared desktop as members' writes of it are decided,
    /// followed from the first such write ([`desktop_writes`]).
    desktop: Option<crate::desktop_guard::DesktopGuard>,
    /// The segments of members' larger desktop writes, until each is whole.
    desktop_segments: collab_types::DocumentReassembler,
}

impl RoomRecord {
    fn governance_messages(&self) -> Vec<Vec<u8>> {
        self.outgoing
            .iter()
            .map(|publication| publication.bytes.clone())
            .collect()
    }
}

impl GovernedRoom {
    fn create(
        dir: &Path,
        name: &str,
        authority: &DeviceCert,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Self, String> {
        let name = valid_name(name)?;
        let room = uuid::Uuid::new_v4().to_string();
        let roster = vec![authority.clone()];
        let mut peer = StatePeer::new(
            &room,
            authority.clone(),
            roster.clone(),
            RevocationList::new(),
            now,
        )?;
        select_exact_admission(&mut peer)?;
        peer.install_seed(
            tilo_peer_state::workspace_seed(
                &room,
                authority,
                &roster,
                name,
                rand::random::<u16>().max(1),
            )?,
            key,
            now,
        )?;
        prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .durable_bootstrap_messages(key, now, 1)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let path = dir.join(format!("{room}.room"));
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let record = RoomRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&room).map_err(|error| error.to_string())?,
            room: room.clone(),
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
            parts,
            path,
            stewardship: stewardship(dir, &room, authority, key),
            record,
            peer,
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
        };
        this.persist()?;
        Ok(this)
    }

    fn open(path: PathBuf, authority: &DeviceCert, key: &SigningKey) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let (version, _) =
            postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
        if version != RECORD_VERSION {
            return Err(format!("room record version {version} is unknown"));
        }
        let record: RoomRecord = postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if record.authority != *authority {
            return Err("room is governed by a different device".into());
        }
        if !crate::room_workspace_id_matches(&record.room, record.workspace_id)
            || record.published > record.outgoing.len()
        {
            return Err("room record is inconsistent".into());
        }
        let mut parts = peer_parts::PeerParts::open(&path)?;
        let mut peer = StatePeer::restore(
            &parts.load(&record.peer)?,
            &record.room,
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
        let stewardship = path
            .parent()
            .and_then(|dir| stewardship(dir, &record.room, authority, key));
        Ok(Self {
            admissions: admissions::Admissions::open(&path)?,
            record,
            peer,
            parts,
            path,
            stewardship,
            publisher: Arc::default(),
            #[cfg(any(feature = "agents", feature = "web"))]
            compaction_wait: 0,
            desktop: None,
            desktop_segments: collab_types::DocumentReassembler::default(),
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

    /// Name the current peer state's parts in the room file, unwritten.
    #[cfg(test)]
    fn store_peer(&mut self) -> Result<(), String> {
        self.record.peer = self.parts.stage(&self.peer)?;
        Ok(())
    }

    fn peer(&self) -> &StatePeer {
        &self.peer
    }

    /// The governance this room published that the relay has not retained,
    /// in order, each with its place reserved in `records`; `published`
    /// moves past them. Stops before a change that does not read, with why.
    fn unpublished(
        &mut self,
        records: &crate::room_records::RoomRecords,
    ) -> (Vec<publisher::Publication>, Result<(), String>) {
        let mut publications = Vec::new();
        while let Some(entry) = self.record.outgoing.get(self.record.published) {
            let channel = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&entry.bytes)
                .map_err(|error| error.to_string())
                .and_then(|message| {
                    neighbour_wire::retention(&message, entry.checkpoint)
                        .map(|(channel, _)| channel)
                        .ok_or_else(|| "room governance message has no retention channel".into())
                });
            let channel = match channel {
                Ok(channel) => channel,
                Err(error) => return (publications, Err(error)),
            };
            publications.push(publisher::Publication::new(
                records,
                self.record.workspace_id,
                Some(self.record.published),
                crate::room_records::HostCommit {
                    body: entry.bytes.clone(),
                    channel,
                    checkpoint: entry.checkpoint,
                },
            ));
            self.record.published = self.record.published.saturating_add(1);
        }
        (publications, Ok(()))
    }

    /// Take a member's write to a record the governor has into its copy.
    /// Not persisted: the relay retains the write, and opening the room
    /// replays it.
    fn follow(&mut self, author: [u8; 32], message: neighbour_wire::NeighbourMsg) {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = &message
        else {
            return;
        };
        if !self.peer.holds_document(app_id, record_id) {
            return;
        }
        if let Err(error) = self.peer.receive_from(
            tilo_peer_state::TransportOrigin::RetainedRelayDelta(author),
            message,
            SystemTime::now(),
        ) {
            tracing::debug!(room = %self.record.room, %error, "governor did not take in a record write");
        }
    }

    fn room(&self) -> &str {
        &self.record.room
    }

    const fn workspace_id(&self) -> u64 {
        self.record.workspace_id
    }

    fn collaboration(&self) -> &collaboration::CollaborationState {
        &self.record.collaboration
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

    /// Commit a governance change of `candidate`: sign it as the room's next
    /// generation, append its publication, and persist it with the
    /// collaboration state `state` derives from the committed room, all in
    /// one write before anything is answered.
    fn commit(
        &mut self,
        mut candidate: StatePeer,
        key: &SigningKey,
        now: SystemTime,
        state: impl FnOnce(&RoomRecord, &StatePeer) -> Result<collaboration::CollaborationState, String>,
    ) -> Result<u64, String> {
        let committing = crate::timing::start();
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            return Err("a room change cannot replace the room".into());
        }
        let preparing = crate::timing::start();
        prepare_governance(&mut candidate, key, now)?;
        crate::timing::window("room.commit.prepare", preparing);
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or("room generation exhausted")?;
        let copying = crate::timing::start();
        let mut record = self.record.clone();
        crate::timing::window("room.commit.record", copying);
        let tail = record
            .outgoing
            .iter()
            .rev()
            .take_while(|publication| !publication.checkpoint)
            .count();
        if tail >= MAX_PUBLICATION_TAIL {
            // Keep unchanged record checkpoints on their own channels; replacing
            // those baselines could discard member writes. A record changed by
            // this commit must still publish at the governance rollover.
            let changed_records = candidate
                .clone()
                .durable_mutation_messages(&self.peer, key, now, generation)?
                .into_iter()
                .filter_map(|message| match message {
                    neighbour_wire::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => Some((app_id, record_id)),
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>();
            record.outgoing = candidate
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
            let messages = candidate.durable_mutation_messages(&self.peer, key, now, generation)?;
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
        self.peer = candidate;
        crate::timing::event(
            "room.governance_commit",
            committing,
            self.record.outgoing.len() as u64,
        );
        Ok(generation)
    }

    fn summary(&self) -> RoomSummary {
        let now = SystemTime::now();
        RoomSummary {
            room: self.record.room.clone(),
            workspace_id: self.record.workspace_id,
            name: self.record.name.clone(),
            generation: self.record.generation,
            members: self
                .peer
                .governance()
                .governed_members(now)
                .unwrap_or_default(),
        }
    }
}

/// `name` trimmed, when it names a workspace: 1–80 characters, none a
/// control character.
fn valid_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if !(1..=80).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err("workspace name must contain 1–80 characters".into());
    }
    Ok(name)
}

/// Keep exact admission evidence in the governor's copy of the room. A
/// member of a room whose channels are stewarded writes its records sealed,
/// and the governor follows those writes into its copy: with exact evidence
/// it keeps each one as the record's ciphertext history and acknowledges
/// it in its next governance change. Without it a sealed write only marks
/// the record encrypted, and every later change of the room (letting the
/// next person in among them) is refused as incomplete encryption state.
/// A room committed before this selection takes it on its next commit.
fn select_exact_admission(peer: &mut StatePeer) -> Result<(), String> {
    peer.select_admission_v5()
        .map_err(|error| format!("room exceeds exact admission bounds: {error}"))
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

/// Whether the room publishes `message` of a commit to its members. The
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
        .ok_or("room governance message has no retention channel")?;
    Ok(Publication {
        bytes: postcard::to_stdvec(message).map_err(|error| error.to_string())?,
        checkpoint: kind == Retention::Checkpoint,
    })
}

/// What the room authority needs from the relay hosting its rooms.
#[derive(Clone)]
pub(crate) struct RoomHost {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) access: crate::daemon::GovernedAccessWriter,
    pub(crate) records: crate::room_records::RoomRecords,
    pub(crate) endpoint: iroh::Endpoint,
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: Arc<std::sync::RwLock<RevocationList>>,
    /// Pre-admissions each room keeps, spent ones included.
    pub(crate) max_pre_admissions: usize,
    /// The live handshake trust policy, for [`crate::room_access::decide`].
    #[cfg(feature = "agents")]
    pub(crate) trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// Agent delegations, for [`crate::room_access::decide`] and for
    /// refusing an agent the administration only a person holds.
    pub(crate) agents: crate::peer::AgentRegistry,
}

/// The rooms this relay governs.
#[derive(Clone)]
pub struct Rooms {
    inner: Arc<RoomsInner>,
}

struct RoomsInner {
    host: RoomHost,
    dir: PathBuf,
    rooms: AsyncMutex<BTreeMap<String, Arc<AsyncMutex<GovernedRoom>>>>,
    calls: Arc<voice::Calls>,
    page_joins: std::sync::Mutex<pages::PageJoins>,
    /// The room UUID after each settled governance decision.
    changes: tokio::sync::broadcast::Sender<String>,
    /// Each record write of a member the governor's copy took in.
    record_writes: tokio::sync::broadcast::Sender<RecordWrite>,
    /// What members asking for content are answered from.
    content: content::ContentSlot,
    /// Channel access requests and acknowledgements waiting for their
    /// room's steward, by room.
    channel_notices: std::sync::Mutex<HashMap<String, Vec<ChannelNotice>>>,
    /// Pre-admitted redemptions waiting for their room's next admission
    /// pass, by room.
    queued_admissions: std::sync::Mutex<HashMap<String, Vec<pages::QueuedAdmission>>>,
    /// Those waiting for their room's next write of the changes it staged,
    /// by room.
    queued_writes: std::sync::Mutex<HashMap<String, Vec<collaboration::Written>>>,
    /// The rooms listed as public workspaces, and how lively each is.
    listings: listing::Listings,
}

/// What a member sent a room's channel steward.
enum ChannelNoticeKind {
    /// An access request, with the route of the channel's record when the
    /// device sent it.
    Request(
        Box<collab_types::MlsAccessRequestV1>,
        Option<Box<collab_types::ChannelRoute>>,
    ),
    Ack(Box<collab_types::MlsGovernanceAckV1>),
}

/// A channel notice and the authenticated device that sent it.
type ChannelNotice = ([u8; 32], ChannelNoticeKind);

/// Notices one room holds for its steward; a device asks again when its
/// request is dropped.
const MAX_QUEUED_CHANNEL_NOTICES: usize = 16_384;

/// One person's place in a governed room, as installed bots compare it
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

/// A member's write to a record of a governed room, after the governor's
/// copy took it in: what wakes the room's installed bots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordWrite {
    /// The room's UUID.
    pub room: String,
    /// The record's app.
    pub app: String,
    /// The record's id on the wire (`""` for the app's primary).
    pub record: String,
}

/// The authority governing one room, as the room's workspace refers to it.
#[derive(Clone)]
pub(crate) struct RoomAuthority {
    rooms: std::sync::Weak<RoomsInner>,
    room: String,
    /// The device governing the room: this relay's own, which also stewards
    /// the room's channels. What only a steward reads goes to it alone.
    governor: [u8; 32],
}

impl RoomAuthority {
    /// The rooms, while the relay still governs them.
    pub(crate) fn rooms(&self) -> Option<Rooms> {
        self.rooms.upgrade().map(|inner| Rooms { inner })
    }

    /// The governed room.
    pub(crate) fn room(&self) -> &str {
        &self.room
    }

    /// The device governing the room, which stewards its channels.
    pub(crate) const fn governor(&self) -> &[u8; 32] {
        &self.governor
    }

    /// An authority governed by `governor`, for a test of the decision.
    #[cfg(test)]
    pub(crate) fn governed_by(room: &str, governor: [u8; 32]) -> Self {
        Self {
            rooms: std::sync::Weak::new(),
            room: room.to_owned(),
            governor,
        }
    }
}

impl std::fmt::Debug for Rooms {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rooms")
            .field("dir", &self.inner.dir)
            .finish_non_exhaustive()
    }
}

impl Rooms {
    /// Open every room recorded under `dir` and bind each to the relay:
    /// its workspace pinned and admitting only governed members, its
    /// unpublished governance retained, its members' new records created.
    pub(crate) async fn open(host: RoomHost, dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut rooms = BTreeMap::new();
        let entries = std::fs::read_dir(&dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_none_or(|extension| extension != "room") {
                continue;
            }
            let room = GovernedRoom::open(path.clone(), &host.cert, &host.key)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            rooms.insert(room.room().to_owned(), Arc::new(AsyncMutex::new(room)));
        }
        let listings = listing::Listings::open(&dir);
        let this = Self {
            inner: Arc::new(RoomsInner {
                host,
                dir,
                rooms: AsyncMutex::new(rooms),
                calls: Arc::default(),
                page_joins: std::sync::Mutex::default(),
                changes: tokio::sync::broadcast::channel(64).0,
                record_writes: tokio::sync::broadcast::channel(256).0,
                content: content::ContentSlot::default(),
                channel_notices: std::sync::Mutex::default(),
                queued_admissions: std::sync::Mutex::default(),
                queued_writes: std::sync::Mutex::default(),
                listings,
            }),
        };
        let rooms = this
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for room in rooms {
            let mut room = room.lock().await;
            this.bind(&room).await?;
            this.settle(&mut room).await?;
            this.recall(&mut room).await?;
            // What the relay's log revoked while the room was closed.
            this.carry_revocations(&mut room, &[]).await?;
        }
        this.watch_calls();
        this.follow_activity();
        Ok(this)
    }

    /// Create a new governed workspace named `name`.
    ///
    /// # Errors
    ///
    /// Returns why the room could not be created, persisted or bound.
    pub async fn create(&self, name: &str) -> Result<RoomSummary, String> {
        let room = GovernedRoom::create(
            &self.inner.dir,
            name,
            &self.inner.host.cert,
            &self.inner.host.key,
            SystemTime::now(),
        )?;
        self.bind(&room).await?;
        let summary = room.summary();
        let room = Arc::new(AsyncMutex::new(room));
        self.inner
            .rooms
            .lock()
            .await
            .insert(summary.room.clone(), Arc::clone(&room));
        self.settle(&mut *room.lock().await).await?;
        tracing::info!(room = %summary.room, workspace_id = summary.workspace_id, "governed room created");
        Ok(summary)
    }

    /// Name `room` `name`, in its governance (what members show) and in
    /// this relay's record of it. Naming it what it is called changes
    /// nothing.
    ///
    /// # Errors
    ///
    /// The room is not governed here, the name is not 1–80 characters, or
    /// the change could not be committed.
    pub async fn rename(&self, room: &str, name: &str) -> Result<RoomSummary, String> {
        let name = valid_name(name)?;
        let state = self.room(room).await.map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        if governed.record.name == name
            && governed.peer.governance().room_name().as_deref() == Some(name)
        {
            return Ok(governed.summary());
        }
        let now = SystemTime::now();
        let key = &self.inner.host.key;
        let mut candidate = governed.peer.clone();
        candidate
            .governance_mut()
            .author_room_name(name, key, now)?;
        let prior = std::mem::replace(&mut governed.record.name, name.to_owned());
        let collaboration = governed.collaboration().clone();
        if let Err(error) = governed.commit(candidate, key, now, |_, _| Ok(collaboration)) {
            governed.record.name = prior;
            return Err(error);
        }
        self.settle(&mut governed).await?;
        Ok(governed.summary())
    }

    /// Every room this relay governs.
    pub async fn list(&self) -> Vec<RoomSummary> {
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut summaries = Vec::with_capacity(rooms.len());
        for room in rooms {
            summaries.push(room.lock().await.summary());
        }
        summaries
    }

    /// The delegated agents on `room`'s roster, acting or not now.
    ///
    /// # Errors
    ///
    /// Returns why the room's roster could not be read.
    pub async fn agents(
        &self,
        room: &str,
    ) -> Result<Vec<tilo_peer_state::GovernedAgent>, CollaborationError> {
        let room = self.room(room).await?;
        let agents = room
            .lock()
            .await
            .peer()
            .governance()
            .governed_agents(SystemTime::now());
        agents.map_err(CollaborationError::new)
    }

    /// The device that governs every room here: this relay's own.
    #[must_use]
    pub fn governor(&self) -> &DeviceCert {
        &self.inner.host.cert
    }

    /// The join service answering invitation redemption for these rooms.
    #[must_use]
    pub fn join_service(&self) -> RoomJoinService {
        RoomJoinService::new(self.clone())
    }

    /// The current role of `device` only when room Write permission is allowed,
    /// including live capability restrictions; `None` otherwise.
    pub(crate) async fn legacy_writer_role(
        &self,
        room: &str,
        device: [u8; 32],
    ) -> Option<collab_types::role::Role> {
        let governed = self.room(room).await.ok()?;
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

    /// Room UUIDs, each sent after a governance decision on that room was
    /// committed and published. A slow receiver may lag; it then re-reads.
    #[must_use]
    pub fn governance_changes(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }

    /// Each member write to a record the governor's copy took in. A slow
    /// receiver may lag; it then re-reads the records it follows.
    #[must_use]
    pub fn record_writes(&self) -> tokio::sync::broadcast::Receiver<RecordWrite> {
        self.inner.record_writes.subscribe()
    }

    /// The governor's copy of record `record` (`""` for the primary) of
    /// `app` in `room`, saved: a record members write sealed is read with
    /// the keys this relay holds as its channel's steward.
    ///
    /// # Errors
    ///
    /// The room is not governed here, or a sealed record cannot be opened.
    pub async fn record_document(
        &self,
        room: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        let state = self.room(room).await?;
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

    /// The `(app, record)` of every record of `room` the governor's copy
    /// holds readable: in the clear, or sealed under channels this relay
    /// stewards ([`Self::record_document`] opens those).
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn record_routes(
        &self,
        room: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        let state = self.room(room).await?;
        let governed = state.lock().await;
        let mut routes = governed.peer().plaintext_routes();
        if governed.stewardship.is_some() {
            routes.extend(governed.peer().sealed_routes());
        }
        drop(governed);
        Ok(routes)
    }

    /// `room` as governance records it now.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn summary(&self, room: &str) -> Result<RoomSummary, CollaborationError> {
        Ok(self.room(room).await?.lock().await.summary())
    }

    /// The spectators watching `room` now: those holding a seat, not every
    /// page that asked for one.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn seated_spectators(&self, room: &str) -> Result<usize, CollaborationError> {
        let workspace_id = self.room(room).await?.lock().await.workspace_id();
        let workspace = self
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned();
        Ok(match workspace {
            Some(workspace) => crate::room_access::seated_spectators(&*workspace.lock().await),
            None => 0,
        })
    }

    /// Every person governance records in `room` other than its owner, by
    /// person (hex root), with the room's governance generation.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn membership(
        &self,
        room: &str,
    ) -> Result<(u64, std::collections::BTreeMap<String, MemberMark>), CollaborationError> {
        let now = SystemTime::now();
        let state = self.room(room).await?;
        let governed = state.lock().await;
        let policy = governed
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)
            .map_err(CollaborationError::new)?
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

    /// The apps `room` lets author its content
    /// ([`tilo_action_policy::roles::app_authorship`]), as its governance
    /// holds them now.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn app_authors(
        &self,
        room: &str,
    ) -> Result<std::collections::BTreeSet<String>, CollaborationError> {
        let now = SystemTime::now();
        let state = self.room(room).await?;
        let policy = state
            .lock()
            .await
            .peer()
            .permissions(self.inner.host.cert.device_public_key, now)
            .map_err(CollaborationError::new)?
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

    async fn room(&self, room: &str) -> Result<Arc<AsyncMutex<GovernedRoom>>, CollaborationError> {
        self.inner
            .rooms
            .lock()
            .await
            .get(room)
            .cloned()
            .ok_or_else(|| CollaborationError::new("this relay does not govern that room"))
    }

    /// Pin the room's workspace, admitting only its governed members, and
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
    async fn bind(&self, room: &GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let workspace_id = room.workspace_id();
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
                let baseline = room
                    .peer()
                    .admitted_room()
                    .ok_or("room governance missing")?
                    .clone()
                    .save();
                store
                    .save_snapshot(&baseline)
                    .map_err(|error| error.to_string())?;
            }
        }
        let workspace = crate::workspace_io::get_or_initialize(
            &host.workspaces,
            host.access.state_root().to_path_buf(),
            host.access.shell_payloads(),
            workspace_id,
            true,
            crate::room_access::AdmissionMode::Governed,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = workspace.lock().await;
        // This relay's room authority governs the room: never auto-enrol,
        // even if the room was autobound before the authority took it.
        if std::mem::replace(
            &mut runtime.admission,
            crate::room_access::AdmissionMode::Governed,
        ) != crate::room_access::AdmissionMode::Governed
        {
            crate::authority_epoch::bump();
        }
        runtime.room_authority = Some(RoomAuthority {
            rooms: Arc::downgrade(&self.inner),
            room: room.room().to_owned(),
            governor: host.cert.device_public_key,
        });
        // The relay stewards the room's channels: what only a steward reads
        // is neither delivered to members nor replayed to them.
        runtime.store.replay_without_steward_only();
        if runtime.governor_tap.is_none() {
            let (tap, notices) = tokio::sync::mpsc::channel(RECORD_NOTICES);
            runtime.governor_tap = Some(tap);
            self.follow(room.room().to_owned(), notices);
        }
        Ok(())
    }

    /// Follow each record write a member makes, see [`Self::absorb`].
    fn follow(&self, room: String, mut notices: tokio::sync::mpsc::Receiver<([u8; 32], Vec<u8>)>) {
        let rooms = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            while let Some((author, body)) = notices.recv().await {
                let Some(inner) = rooms.upgrade() else { break };
                if let Err(error) = (Self { inner }).absorb(&room, author, &body).await {
                    tracing::warn!(%room, %error, "governed room did not follow a record write");
                }
            }
        });
    }

    /// Take in the record write `body` of `author`. A record the room lacks
    /// is created from it, if `author` is a device of an editor: records are
    /// born only from a governor checkpoint. A write to a record the room has
    /// updates the governor's copy only in memory; opening the room replays
    /// what the relay retains ([`Self::recall`]).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its decision is committed, published and answered"
    )]
    async fn absorb(&self, room: &str, author: [u8; 32], body: &[u8]) -> Result<(), String> {
        let host = &self.inner.host;
        if author == host.cert.device_public_key {
            return Ok(());
        }
        let Ok(message) = postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body) else {
            return Ok(());
        };
        match message {
            neighbour_wire::NeighbourMsg::MlsAccessRequest {
                room: named,
                request,
                ..
            } if named == room => {
                return self
                    .channel_notice(room, (author, ChannelNoticeKind::Request(request, None)));
            }
            neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest {
                room: named,
                request,
                route,
                ..
            } if named == room => {
                return self.channel_notice(
                    room,
                    (author, ChannelNoticeKind::Request(request, Some(route))),
                );
            }
            neighbour_wire::NeighbourMsg::MlsGovernanceAck {
                room: named, ack, ..
            } if named == room => {
                return self.channel_notice(room, (author, ChannelNoticeKind::Ack(ack)));
            }
            // The transport authenticated the author, but the entries are
            // the roots' own signatures: who carries them does not matter.
            neighbour_wire::NeighbourMsg::IdentityHello { revocations, .. } => {
                return self.take_revocations(revocations).await;
            }
            _ => {}
        }
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
        let room = self.room(room).await.map_err(|error| error.to_string())?;
        let waiting = crate::timing::start();
        let mut room = room.lock().await;
        crate::timing::window("room.follow_wait", waiting);
        if room.peer().holds_document(&app_id, &record_id) {
            self.follow_write(&mut room, author, message, (app_id, record_id))
                .await;
            return Ok(());
        }
        let sealed = envelope.encryption.is_some();
        // Only the record's channel members read a sealed write: unless this
        // relay stewards that channel, it retains the write and the governor
        // keeps no copy. A record that starts sealed, as each new part of a
        // chained record does, is followed like any other where it does.
        #[cfg(any(feature = "agents", feature = "web"))]
        let readable = !sealed || room.stewards_channel(&app_id, &record_id).await;
        #[cfg(not(any(feature = "agents", feature = "web")))]
        let readable = !sealed;
        if !readable {
            return Ok(());
        }
        let now = SystemTime::now();
        let member = room.peer().permissions(author, now)?;
        let node = collab_types::validator::document_node(&member.policy.room, &app_id, &record_id);
        if !member.allows(&node, tilo_action_policy::ActionKind::Undoable, now) {
            return Err(format!(
                "{app_id} record written by a device that may not edit"
            ));
        }
        // The checkpoint includes the write that created the record: the
        // relay retains a member's changes only as a tail of a checkpoint
        // that resolves their dependencies.
        let mut candidate = room.peer().clone();
        candidate.publish_document(
            &app_id,
            &record_id,
            automerge::AutoCommit::new().save(),
            &host.key,
            now,
        )?;
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
            candidate.publish_document(&app_id, &record_id, written, &host.key, now)?;
        }
        room.commit(candidate, &host.key, now, |record, _| {
            Ok(record.collaboration.clone())
        })?;
        self.settle(&mut room).await
    }

    /// Queue `notice` for `room`'s steward. True when it is the first since
    /// the steward last took them, so its sender must hand them over.
    fn queue_channel_notice(&self, room: &str, notice: ChannelNotice) -> Result<bool, String> {
        let mut queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        let waiting = queued.entry(room.to_owned()).or_default();
        let first = waiting.len() < MAX_QUEUED_CHANNEL_NOTICES && {
            waiting.push(notice);
            waiting.len() == 1
        };
        drop(queued);
        Ok(first)
    }

    /// Take in `author`'s access request or acknowledgement for a channel of
    /// `room`, for its steward.
    ///
    /// A room follows its members' writes one at a time, and a crowd joining
    /// sends a request each and an acknowledgement per member per change.
    /// So notices only queue here; the first since the steward last took
    /// them starts a task that takes the room once for all of them.
    fn channel_notice(&self, room: &str, notice: ChannelNotice) -> Result<(), String> {
        if self.queue_channel_notice(room, notice)? {
            let rooms = self.clone();
            let room = room.to_owned();
            tokio::spawn(async move {
                if let Err(error) = rooms.steward_notices(&room).await {
                    tracing::warn!(%room, %error, "channel steward did not take its notices");
                }
            });
        }
        Ok(())
    }

    /// Hand `room`'s steward every queued notice: record the
    /// acknowledgements, then admit every device asking in one change per
    /// channel, and hold the channels until that change is durable.
    ///
    /// Each change is an epoch every member applies before it reads what
    /// was written after it, and a member that joined from an admission
    /// the host replaced before it arrived asks again only after seconds.
    /// While one change is written, the requests that arrive wait for the
    /// next one, which admits all of them: a crowd joins in a few changes,
    /// not one per pass.
    async fn steward_notices(&self, room: &str) -> Result<(), String> {
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let (workspace_id, generation, readers, steward, publisher) = {
            let governed = governed.lock().await;
            let Some(steward) = governed.stewardship.clone() else {
                self.inner
                    .channel_notices
                    .lock()
                    .map_err(|_| "channel notices poisoned")?
                    .remove(room);
                return Ok(());
            };
            let now = SystemTime::now();
            let access = governed.peer().governance().governed_access(now)?;
            self.learn_queued_routes(room, &steward, &access)?;
            let room_readers = readers(governed.peer(), &access, Some(&*steward), now)?;
            let taken = (
                governed.workspace_id(),
                governed.record.generation,
                room_readers,
                steward,
                Arc::clone(&governed.publisher),
            );
            drop(governed);
            taken
        };
        {
            let stewardship = Arc::clone(&steward.channels).lock_owned().await;
            // Taken under the steward: notices that queue while it works
            // start the next pass, which takes all of them at once.
            let waiting = self
                .inner
                .channel_notices
                .lock()
                .map_err(|_| "channel notices poisoned")?
                .remove(room)
                .unwrap_or_default();
            let readers = steward.readers(generation, readers);
            let passing = crate::timing::start();
            let named = room.to_owned();
            // Verifying requests and the MLS changes admitting them run on a
            // blocking worker: a crowd's took seconds, and held a runtime
            // worker (and every task queued on it) the whole time.
            let (stewardship, admitted) = tokio::task::spawn_blocking(move || {
                let mut stewardship = stewardship;
                let admitted = take_notices(&mut stewardship, &named, waiting, &readers, passing);
                (stewardship, admitted)
            })
            .await
            .map_err(|error| error.to_string())?;
            let Some((messages, requests)) = admitted? else {
                return Ok(());
            };
            // After the channel changes a settle handed over.
            publisher.flushed().await;
            let admissions = self
                .enqueue_channel_governance(workspace_id, messages)
                .await?;
            for admission in admissions {
                admission.durable().await?;
            }
            crate::timing::event("steward.pass", passing, requests);
            drop(stewardship);
        }
        Ok(())
    }

    /// Keep the route of every routed access request queued for `room`'s
    /// `steward` whose author is a governed member device (`access`) and
    /// whose request is its own: the readers the queued requests are decided
    /// with then decide each named record itself. A route that does not
    /// prove its channel is not kept ([`stewardship::Steward::learn_route`]),
    /// and the request carrying it is refused when it is taken.
    fn learn_queued_routes(
        &self,
        room: &str,
        steward: &stewardship::Steward,
        access: &GovernedAccess,
    ) -> Result<(), String> {
        let queued = self
            .inner
            .channel_notices
            .lock()
            .map_err(|_| "channel notices poisoned")?;
        for (author, notice) in queued.get(room).into_iter().flatten() {
            if let ChannelNoticeKind::Request(request, Some(route)) = notice
                && request.device.as_bytes() == author
                && access.member_devices.contains(author)
            {
                steward.learn_route(request.channel, route);
            }
        }
        drop(queued);
        Ok(())
    }

    /// Retain channel governance messages as this relay's own.
    #[cfg(feature = "agents")]
    async fn publish_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<(), String> {
        for admission in self
            .enqueue_channel_governance(workspace_id, messages)
            .await?
        {
            admission.durable().await?;
        }
        Ok(())
    }

    /// Take `message`, `author`'s write to the record `(app, record)` the
    /// governor holds, into its copy, compact the room's sealed histories
    /// before one is full (past its bound the copy would stop following
    /// the record), and tell whoever follows the room's records.
    #[cfg_attr(
        not(any(feature = "agents", feature = "web")),
        expect(
            clippy::unused_async,
            reason = "only a relay that stewards channels compacts what it follows"
        )
    )]
    async fn follow_write(
        &self,
        room: &mut GovernedRoom,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
        (app, record): (String, String),
    ) {
        let following = crate::timing::start();
        room.follow(author, message);
        crate::timing::window("room.follow", following);
        #[cfg(any(feature = "agents", feature = "web"))]
        {
            let compacting = crate::timing::start();
            if let Err(error) = self.compact_sealed_histories(room).await {
                tracing::warn!(room = %room.room(), %error, "governed room did not compact a sealed record");
            }
            crate::timing::window("room.compact", compacting);
        }
        let _ = self.inner.record_writes.send(RecordWrite {
            room: room.room().to_owned(),
            app,
            record,
        });
    }

    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<neighbour_wire::NeighbourMsg>,
    ) -> Result<Vec<crate::room_records::RoomAdmission>, String> {
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

    /// Replay every record write the relay retains for `room` into the
    /// governor's copy.
    async fn recall(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let retained = self
            .inner
            .host
            .records
            .retained(room.workspace_id())
            .await?;
        for retained in retained {
            if retained.author == self.inner.host.cert.device_public_key {
                continue;
            }
            if let Ok(message) = postcard::from_bytes(&retained.body) {
                room.follow(retained.author, message);
            }
        }
        Ok(())
    }

    /// Bring the relay in line with the room's committed state: admit exactly
    /// its governed members, then retain every governance change not yet
    /// published, then re-derive its calls' participants. Runs after each
    /// decision, before the decision is answered.
    ///
    /// The changes are handed to the room's [`publisher::RoomPublisher`]
    /// here, under the room's lock, in publication order, each with its
    /// place reserved so that whoever reads the room's retention waits for
    /// it. The publisher admits them to the host's order and waits for them
    /// to be durable after the lock is released: the next decision waits
    /// neither for room in the workspace owner's queue, which a crowd fills,
    /// nor behind this one's disk writes (and every member write the host
    /// queued before them). A change the publisher could not admit is
    /// published again by the next settle.
    async fn settle(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let now = SystemTime::now();
        let access: GovernedAccess = room.peer().governance().governed_access(now)?;
        let readers = readers(room.peer(), &access, room.stewardship.as_deref(), now)?;
        host.access
            .replace(room.workspace_id(), true, access)
            .await
            .map_err(|error| error.to_string())?;
        host.access
            .replace_standings(
                room.workspace_id(),
                true,
                room.peer()
                    .governance()
                    .governed_standings(SystemTime::now())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let publisher = Arc::clone(&room.publisher);
        let epoch = publisher.reconcile(&mut room.record.published);
        let workspace_id = room.workspace_id();
        let (mut publications, unreadable) = room.unpublished(&host.records);
        if let Err(error) = unreadable {
            // What precedes it is still published, in order.
            publisher.push(
                &host.records,
                room.room(),
                workspace_id,
                epoch,
                publications,
            );
            return Err(error);
        }
        match room.stewardship.clone() {
            // A join only lets a device in, which moves no channel: the
            // room does not wait for an admission in progress.
            Some(steward) if steward.moves_channels(&readers) => {
                let mut channels = steward.channels.lock().await;
                let commits = channels.reconcile_all(&readers).and_then(|messages| {
                    steward.hand_over(room.record.generation, &readers);
                    messages
                        .iter()
                        .map(channel_commit)
                        .collect::<Result<Vec<_>, _>>()
                });
                // Handed over under the steward, which orders its channels'
                // changes: whoever admits one next waits for these first.
                // The room's own changes go first either way.
                for commit in commits.iter().flatten().cloned() {
                    publications.push(publisher::Publication::new(
                        &host.records,
                        workspace_id,
                        None,
                        commit,
                    ));
                }
                publisher.push(
                    &host.records,
                    room.room(),
                    workspace_id,
                    epoch,
                    publications,
                );
                drop(channels);
                commits?;
            }
            steward => {
                if let Some(steward) = steward {
                    steward.hand_over(room.record.generation, &readers);
                }
                publisher.push(
                    &host.records,
                    room.room(),
                    workspace_id,
                    epoch,
                    publications,
                );
            }
        }
        self.observe_calls(room);
        // Watchers (a persistent room's operator) learn that this room's
        // governance moved; nobody listening is fine.
        let _ = self.inner.changes.send(room.room().to_owned());
        Ok(())
    }

    /// The signed Iroh address a joining device dials to redeem an
    /// invitation of `room`.
    fn transport_hint(
        &self,
        room: &GovernedRoom,
    ) -> Result<tilo_peer_state::join::TransportHint, CollaborationError> {
        let host = &self.inner.host;
        let endpoint_addr = host.endpoint.addr();
        let packet_relay_url = endpoint_addr
            .relay_urls()
            .next()
            .map_or_else(|| "iroh:direct".to_owned(), ToString::to_string);
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
            version: 1,
            mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::CertifiedHost,
            room: room.room().to_owned(),
            workspace_id: room.workspace_id(),
            governing_authority: room.peer().authority().clone(),
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

/// Hand `room`'s `stewardship` the notices `waiting` for it: take each
/// request, record the acknowledgements, then admit every device asking.
/// Synchronous MLS work, for a blocking worker. The governance messages to
/// publish and how many requests they answer; `None` when nobody asked.
fn take_notices(
    stewardship: &mut stewardship::Stewardship,
    room: &str,
    waiting: Vec<ChannelNotice>,
    readers: &stewardship::Readers,
    passing: Option<std::time::Instant>,
) -> Result<Option<(Vec<neighbour_wire::NeighbourMsg>, u64)>, String> {
    #[cfg(test)]
    tests::steward_pass_hook(room);
    let mut asked = false;
    let mut acks = Vec::new();
    let mut requests = 0u64;
    for (author, notice) in waiting {
        match notice {
            ChannelNoticeKind::Request(request, route) => {
                asked = true;
                requests = requests.saturating_add(1);
                if let Err(error) = stewardship.request(author, *request, route.as_deref(), readers)
                {
                    tracing::debug!(room, %error, "channel access request refused");
                }
            }
            ChannelNoticeKind::Ack(ack) => acks.push((author, *ack)),
        }
    }
    crate::timing::event("steward.acks", passing, acks.len() as u64);
    stewardship.acknowledge(acks);
    if !asked {
        return Ok(None);
    }
    let messages = stewardship.admit_pending(readers)?;
    crate::timing::event("steward.admit", passing, requests);
    Ok(Some((messages, requests)))
}

/// A channel governance message, retained as this relay's own.
fn channel_commit(
    message: &neighbour_wire::NeighbourMsg,
) -> Result<crate::room_records::HostCommit, String> {
    let (channel, retention) = neighbour_wire::retention(message, false)
        .ok_or("channel governance has no retention channel")?;
    let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
    Ok(crate::room_records::HostCommit {
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
