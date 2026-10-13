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
//! Devices of the relay's own user administer its rooms over
//! [`ROOM_ADMIN_ALPN`] or, from a page, over HTTPS; any device redeems an
//! invitation over the shared workspace-join protocol, and a page over
//! HTTPS, where the owner then admits it.

mod admin;
mod admissions;
#[cfg(feature = "agents")]
mod agent_members;
#[cfg(feature = "agents")]
mod applications;
mod collaboration;
mod content;
pub use content::RoomContent;
mod pages;
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
use serde::{Deserialize, Serialize};
use tilo_peer_state::{GovernedAccess, StatePeer, transport::Retention};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

pub use admin::RoomAdminProtocol;
pub(crate) use admissions::DEFAULT_MAX_PRE_ADMISSIONS;
pub use collaboration::{
    CollaborationError, GovernanceCompletion, GrantAppAuthorshipRequest, RoomJoinService,
};
pub use pages::PreAdmitRequest;
// The administration wire vocabulary and the device's client, under the
// daemon's historic paths.
pub use tilo_relay_session::rooms::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    ROOM_ADMIN_ALPN, RemoveMemberRequest, RoomAdminClient, RoomAdminError, RoomCommand, RoomReply,
    RoomSummary, SetMemberRoleRequest,
};

use crate::peer::WorkspaceRegistry;

const RECORD_VERSION: u32 = 1;
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

/// Everything a governed room keeps across restarts. Written whole and
/// atomically after each decision, before it is published or answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct RoomRecord {
    version: u32,
    room: String,
    workspace_id: u64,
    name: String,
    authority: DeviceCert,
    peer: Vec<u8>,
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

/// The devices governance lets read a room's channels.
fn readers(access: &GovernedAccess) -> std::collections::BTreeSet<collab_types::Ed25519PublicKey> {
    access
        .member_devices
        .iter()
        .map(|device| collab_types::Ed25519PublicKey::from_bytes(*device))
        .collect()
}

/// The stewards of `room`'s channels, under `dir`, when `authority` (this
/// relay) may own workspaces.
fn stewardship(
    dir: &Path,
    room: &str,
    authority: &DeviceCert,
    key: &SigningKey,
) -> Option<stewardship::Stewardship> {
    authority
        .capabilities
        .can_own_workspaces
        .then(|| stewardship::Stewardship::new(room, dir.join(format!("{room}.channels")), key))
}

/// A room this relay governs.
struct GovernedRoom {
    record: RoomRecord,
    peer: StatePeer,
    path: PathBuf,
    /// Its channels' stewards, when this relay is a node that may own
    /// workspaces ([`stewardship`]).
    stewardship: Option<stewardship::Stewardship>,
    /// The owner's pre-admissions and notices.
    admissions: admissions::Admissions,
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
        peer.install_seed(empty_workspace(&room, authority, &roster, name)?, key, now)?;
        prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .durable_bootstrap_messages(key, now, 1)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let record = RoomRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&room).map_err(|error| error.to_string())?,
            room: room.clone(),
            name: name.to_owned(),
            authority: authority.clone(),
            peer: peer.durable()?,
            generation: 1,
            outgoing,
            published: 0,
            collaboration: collaboration::CollaborationState::default(),
        };
        let path = dir.join(format!("{room}.room"));
        let this = Self {
            admissions: admissions::Admissions::open(&path)?,
            path,
            stewardship: stewardship(dir, &room, authority, key),
            record,
            peer,
        };
        this.persist()?;
        Ok(this)
    }

    fn open(path: PathBuf, authority: &DeviceCert, key: &SigningKey) -> Result<Self, String> {
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let record: RoomRecord = postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        if record.version != RECORD_VERSION {
            return Err(format!("room record version {} is unknown", record.version));
        }
        if record.authority != *authority {
            return Err("room is governed by a different device".into());
        }
        if !crate::room_workspace_id_matches(&record.room, record.workspace_id)
            || record.published > record.outgoing.len()
        {
            return Err("room record is inconsistent".into());
        }
        let peer = StatePeer::restore(
            &record.peer,
            &record.room,
            record.authority.clone(),
            vec![record.authority.clone()],
            RevocationList::new(),
            SystemTime::now(),
        )?;
        if !peer.has_governance() {
            return Err("restored room has no governance".into());
        }
        let stewardship = path
            .parent()
            .and_then(|dir| stewardship(dir, &record.room, authority, key));
        Ok(Self {
            admissions: admissions::Admissions::open(&path)?,
            record,
            peer,
            path,
            stewardship,
        })
    }

    fn persist(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(&self.record).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.path, &bytes).map_err(|error| error.to_string())
    }

    fn peer(&self) -> &StatePeer {
        &self.peer
    }

    /// Take a member's write to a record the governor has into its copy.
    /// Not persisted: the relay retains the write, and opening the room
    /// replays it.
    fn follow(&mut self, author: [u8; 32], message: tilo_peer_state::NeighbourMsg) {
        let tilo_peer_state::NeighbourMsg::AppDocument {
            app_id, record_id, ..
        } = &message
        else {
            return;
        };
        if self.peer.document(app_id, record_id).is_none() {
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
        if candidate.room() != self.peer.room() || candidate.authority() != self.peer.authority() {
            return Err("a room change cannot replace the room".into());
        }
        prepare_governance(&mut candidate, key, now)?;
        let generation = self
            .record
            .generation
            .checked_add(1)
            .ok_or("room generation exhausted")?;
        let mut record = self.record.clone();
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
                    tilo_peer_state::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => Some((app_id, record_id)),
                    _ => None,
                })
                .collect::<std::collections::BTreeSet<_>>();
            record.outgoing = candidate
                .durable_bootstrap_messages(key, now, generation)?
                .iter()
                .filter(|message| match message {
                    tilo_peer_state::NeighbourMsg::AppDocument {
                        app_id, record_id, ..
                    } => changed_records.contains(&(app_id.clone(), record_id.clone())),
                    _ => true,
                })
                .map(|message| publication(message, true))
                .collect::<Result<_, _>>()?;
            record.published = 0;
        } else {
            let messages = candidate.durable_mutation_messages(&self.peer, key, now, generation)?;
            for message in &messages {
                record.outgoing.push(publication(message, false)?);
            }
        }
        record.peer = candidate.durable()?;
        record.generation = generation;
        record.collaboration = state(&record, &candidate)?;
        let prior = std::mem::replace(&mut self.record, record);
        if let Err(error) = self.persist() {
            self.record = prior;
            return Err(error);
        }
        self.peer = candidate;
        Ok(generation)
    }

    fn summary(&self) -> RoomSummary {
        let now = SystemTime::now();
        RoomSummary {
            room: self.record.room.clone(),
            workspace_id: self.record.workspace_id,
            name: self.record.name.clone(),
            generation: self.record.generation,
            members: self.peer.governed_members(now).unwrap_or_default(),
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

fn prepare_governance(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: SystemTime,
) -> Result<(), String> {
    peer.reconcile_media_governance(key, now)?;
    peer.validate_media_governance_for_publication()
}

fn publication(
    message: &tilo_peer_state::NeighbourMsg,
    checkpoint: bool,
) -> Result<Publication, String> {
    let (_, kind) = tilo_peer_state::transport::retention(message, checkpoint)
        .ok_or("room governance message has no retention channel")?;
    Ok(Publication {
        bytes: postcard::to_stdvec(message).map_err(|error| error.to_string())?,
        checkpoint: kind == Retention::Checkpoint,
    })
}

/// A new workspace: governance and topology custody, no app records.
fn empty_workspace(
    room: &str,
    authority: &DeviceCert,
    roster: &[DeviceCert],
    name: &str,
) -> Result<tilo_peer_state::SeedState, String> {
    use automerge::{AutoCommit, Change, ObjType, ROOT, transaction::Transactable};
    use tilo_automerge::ApplyIncoming as _;

    let mut doc = AutoCommit::new().with_actor(automerge::ActorId::from(
        authority.device_public_key.to_vec(),
    ));
    let changes = tilo_peer_state::seed_room(room, authority, roster)?
        .into_iter()
        .map(Change::from_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    doc.apply_incoming(changes)
        .map_err(|error| error.to_string())?;
    doc.put(ROOT, "name", name)
        .map_err(|error| error.to_string())?;
    doc.put_object(ROOT, "contents", ObjType::Map)
        .map_err(|error| error.to_string())?;
    let owners = doc
        .put_object(ROOT, "contents_owners_v1", ObjType::Map)
        .map_err(|error| error.to_string())?;
    let salt = rand::random::<u16>().max(1);
    doc.put(
        owners,
        format!("{salt:04x}"),
        hex::encode(authority.device_public_key),
    )
    .map_err(|error| error.to_string())?;
    doc.put(ROOT, "desktop_counter_v1", 1_u64)
        .map_err(|error| error.to_string())?;
    doc.commit();
    Ok(tilo_peer_state::SeedState {
        room_changes: doc
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        documents: vec![],
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
    /// Agent delegations, for [`crate::room_access::decide`].
    #[cfg(feature = "agents")]
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
}

/// What a member sent a room's channel steward.
enum ChannelNoticeKind {
    Request(Box<collab_types::MlsAccessRequestV1>),
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
    /// the room's channels. Read by the steward-key decision only.
    #[cfg(any(test, feature = "agents"))]
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
    #[cfg(any(test, feature = "agents"))]
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
        }
        this.watch_calls();
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
        if governed.record.name == name && governed.peer.room_name().as_deref() == Some(name) {
            return Ok(governed.summary());
        }
        let now = SystemTime::now();
        let key = &self.inner.host.key;
        let mut candidate = governed.peer.clone();
        candidate.author_room_name(name, key, now)?;
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
        let agents = room.lock().await.peer().governed_agents(SystemTime::now());
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
    /// `app` in `room`, saved, when the room holds it in plaintext.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn record_document(
        &self,
        room: &str,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, CollaborationError> {
        Ok(self
            .room(room)
            .await?
            .lock()
            .await
            .peer()
            .document(app, record))
    }

    /// The `(app, record)` of every record of `room` the governor's copy
    /// holds readable.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn record_routes(
        &self,
        room: &str,
    ) -> Result<Vec<(String, String)>, CollaborationError> {
        Ok(self
            .room(room)
            .await?
            .lock()
            .await
            .peer()
            .plaintext_routes())
    }

    /// `room` as governance records it now.
    ///
    /// # Errors
    ///
    /// The room is not governed here.
    pub async fn summary(&self, room: &str) -> Result<RoomSummary, CollaborationError> {
        Ok(self.room(room).await?.lock().await.summary())
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
            let store =
                crate::store::WorkspaceStore::open(host.access.state_root(), workspace_id, true)
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
            #[cfg(any(test, feature = "agents"))]
            governor: host.cert.device_public_key,
        });
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
        let Ok(message) = postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(body) else {
            return Ok(());
        };
        match message {
            tilo_peer_state::NeighbourMsg::MlsAccessRequest {
                room: named,
                request,
                ..
            } if named == room => {
                return self.channel_notice(room, (author, ChannelNoticeKind::Request(request)));
            }
            tilo_peer_state::NeighbourMsg::MlsGovernanceAck {
                room: named, ack, ..
            } if named == room => {
                return self.channel_notice(room, (author, ChannelNoticeKind::Ack(ack)));
            }
            _ => {}
        }
        let tilo_peer_state::NeighbourMsg::AppDocument {
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
        let mut room = room.lock().await;
        if room.peer().document(&app_id, &record_id).is_some() {
            room.follow(author, message);
            let _ = self.inner.record_writes.send(RecordWrite {
                room: room.room().to_owned(),
                app: app_id,
                record: record_id,
            });
            return Ok(());
        }
        if envelope.encryption.is_some() {
            // Only the record's channel members read it: the relay retains
            // the sealed write, and the governor keeps no copy.
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
        let written = candidate
            .document(&app_id, &record_id)
            .ok_or("created record missing")?;
        candidate.publish_document(&app_id, &record_id, written, &host.key, now)?;
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
    /// channel, queued in the host's order before the room is let go; wait
    /// for the change to be durable only after that.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the room stays locked until its channel change holds its place in the host's order"
    )]
    async fn steward_notices(&self, room: &str) -> Result<(), String> {
        let governed = self.room(room).await.map_err(|error| error.to_string())?;
        let admissions = {
            let mut governed = governed.lock().await;
            let waiting = self
                .inner
                .channel_notices
                .lock()
                .map_err(|_| "channel notices poisoned")?
                .remove(room)
                .unwrap_or_default();
            let workspace_id = governed.workspace_id();
            let readers = readers(&governed.peer().governed_access(SystemTime::now())?);
            let Some(stewardship) = governed.stewardship.as_mut() else {
                return Ok(());
            };
            let mut asked = false;
            let mut acks = Vec::new();
            for (author, notice) in waiting {
                match notice {
                    ChannelNoticeKind::Request(request) => {
                        asked = true;
                        if let Err(error) = stewardship.request(author, *request, &readers) {
                            tracing::debug!(room, %error, "channel access request refused");
                        }
                    }
                    ChannelNoticeKind::Ack(ack) => acks.push((author, *ack)),
                }
            }
            stewardship.acknowledge(acks);
            if !asked {
                return Ok(());
            }
            let messages = stewardship.admit_pending(&readers)?;
            self.enqueue_channel_governance(workspace_id, messages)
                .await?
        };
        for admission in admissions {
            admission.durable().await?;
        }
        Ok(())
    }

    /// Retain channel governance messages as this relay's own.
    #[cfg(feature = "agents")]
    async fn publish_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<tilo_peer_state::NeighbourMsg>,
    ) -> Result<(), String> {
        for admission in self
            .enqueue_channel_governance(workspace_id, messages)
            .await?
        {
            admission.durable().await?;
        }
        Ok(())
    }

    /// Queue channel governance messages in the host's order as this
    /// relay's own, without waiting for them to be durable.
    async fn enqueue_channel_governance(
        &self,
        workspace_id: u64,
        messages: Vec<tilo_peer_state::NeighbourMsg>,
    ) -> Result<Vec<crate::room_records::RoomAdmission>, String> {
        let mut admissions = Vec::with_capacity(messages.len());
        for message in messages {
            let (channel, retention) = neighbour_wire::retention(&message, false)
                .ok_or("channel governance has no retention channel")?;
            let body = postcard::to_stdvec(&message).map_err(|error| error.to_string())?;
            admissions.push(
                self.inner
                    .host
                    .records
                    .enqueue(
                        workspace_id,
                        crate::room_records::HostCommit {
                            body,
                            channel,
                            checkpoint: retention == neighbour_wire::Retention::Checkpoint,
                        },
                    )
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
    /// The changes take their place in the host's order here, under the
    /// room's lock; they become durable after it, so the next decision does
    /// not wait behind this one's disk writes (and every member write the
    /// host queued before them).
    async fn settle(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let access: GovernedAccess = room.peer().governed_access(SystemTime::now())?;
        let readers = readers(&access);
        host.access
            .replace(room.workspace_id(), true, access)
            .await
            .map_err(|error| error.to_string())?;
        host.access
            .replace_standings(
                room.workspace_id(),
                true,
                room.peer().governed_standings(SystemTime::now())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let mut admissions = Vec::new();
        while let Some(entry) = room.record.outgoing.get(room.record.published) {
            let message: tilo_peer_state::NeighbourMsg =
                postcard::from_bytes(&entry.bytes).map_err(|error| error.to_string())?;
            let (channel, _) = tilo_peer_state::transport::retention(&message, entry.checkpoint)
                .ok_or("room governance message has no retention channel")?;
            admissions.push(
                host.records
                    .enqueue(
                        room.workspace_id(),
                        crate::room_records::HostCommit {
                            body: entry.bytes.clone(),
                            channel,
                            checkpoint: entry.checkpoint,
                        },
                    )
                    .await?,
            );
            room.record.published = room.record.published.saturating_add(1);
        }
        if let Some(stewardship) = room.stewardship.as_mut() {
            let messages = stewardship.reconcile_all(&readers)?;
            admissions.extend(
                self.enqueue_channel_governance(room.workspace_id(), messages)
                    .await?,
            );
        }
        if !admissions.is_empty() {
            let name = room.room().to_owned();
            tokio::spawn(async move {
                for admission in admissions {
                    if let Err(error) = admission.durable().await {
                        tracing::warn!(room = %name, %error, "room governance change was not retained");
                        break;
                    }
                }
            });
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
        self.inner
            .host
            .revocations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}
