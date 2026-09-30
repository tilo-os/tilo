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
//! [`ROOM_ADMIN_ALPN`]; any device redeems an invitation over the shared
//! workspace-join protocol.

mod admin;
mod collaboration;
#[cfg(test)]
mod tests;
mod voice;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::{GovernedAccess, StatePeer, transport::Retention};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

pub use admin::{
    ROOM_ADMIN_ALPN, RoomAdminClient, RoomAdminError, RoomAdminProtocol, RoomCommand, RoomReply,
    RoomSummary,
};
pub use collaboration::{
    CancelInvitationRequest, CollaborationError, GovernanceCompletion, IssueInvitationRequest,
    IssuedInvitation, PendingInvitation, RemoveMemberRequest, RoomJoinService,
    SetMemberRoleRequest,
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
    /// How much of `outgoing` the relay has retained. A crash between the
    /// relay's commit and this record republishes, which members tolerate.
    published: usize,
    collaboration: collaboration::CollaborationState,
}

/// A room this relay governs.
struct GovernedRoom {
    record: RoomRecord,
    peer: StatePeer,
    path: PathBuf,
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
        let name = name.trim();
        if !(1..=80).contains(&name.chars().count()) || name.chars().any(char::is_control) {
            return Err("workspace name must contain 1–80 characters".into());
        }
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
        let this = Self {
            path: dir.join(format!("{room}.room")),
            record,
            peer,
        };
        this.persist()?;
        Ok(this)
    }

    fn open(path: PathBuf, authority: &DeviceCert) -> Result<Self, String> {
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
        Ok(Self { record, peer, path })
    }

    fn persist(&self) -> Result<(), String> {
        let bytes = postcard::to_stdvec(&self.record).map_err(|error| error.to_string())?;
        atomic_file::write(&self.path, &bytes).map_err(|error| error.to_string())
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
            // Record checkpoints stay retained on their own channels: the
            // governor's copy of a record is only the empty one it created, and
            // republishing it would replace every member write since.
            record.outgoing = candidate
                .durable_bootstrap_messages(key, now, generation)?
                .iter()
                .filter(|message| {
                    !matches!(message, tilo_peer_state::NeighbourMsg::AppDocument { .. })
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
}

/// The authority governing one room, as the room's workspace refers to it.
#[derive(Clone)]
pub(crate) struct RoomAuthority {
    rooms: std::sync::Weak<RoomsInner>,
    room: String,
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
            let room = GovernedRoom::open(path.clone(), &host.cert)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            rooms.insert(room.room().to_owned(), Arc::new(AsyncMutex::new(room)));
        }
        let this = Self {
            inner: Arc::new(RoomsInner {
                host,
                dir,
                rooms: AsyncMutex::new(rooms),
                calls: Arc::default(),
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
        let host = &self.inner.host;
        let room = GovernedRoom::create(
            &self.inner.dir,
            name,
            &host.cert,
            &host.key,
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
            false,
            crate::peer::MAX_AUTOBOUND_WORKSPACES,
        )
        .await
        .map_err(|error| error.to_string())?;
        let mut runtime = workspace.lock().await;
        runtime.admit_authenticated = false;
        runtime.room_authority = Some(RoomAuthority {
            rooms: Arc::downgrade(&self.inner),
            room: room.room().to_owned(),
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
            return Ok(());
        }
        if envelope.encryption.is_some() {
            return Err(format!(
                "{app_id} wrote a sealed record the room cannot create"
            ));
        }
        let now = SystemTime::now();
        let (role, ..) = room.peer().permissions(author, now)?;
        if !matches!(
            role,
            collab_types::role::Role::Owner | collab_types::role::Role::Editor
        ) {
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
    async fn settle(&self, room: &mut GovernedRoom) -> Result<(), String> {
        let host = &self.inner.host;
        let access: GovernedAccess = room.peer().governed_access(SystemTime::now())?;
        host.access
            .replace(room.workspace_id(), true, access)
            .await
            .map_err(|error| error.to_string())?;
        while let Some(entry) = room.record.outgoing.get(room.record.published) {
            let message: tilo_peer_state::NeighbourMsg =
                postcard::from_bytes(&entry.bytes).map_err(|error| error.to_string())?;
            let (channel, _) = tilo_peer_state::transport::retention(&message, entry.checkpoint)
                .ok_or("room governance message has no retention channel")?;
            host.records
                .commit(
                    room.workspace_id(),
                    crate::room_records::HostCommit {
                        body: entry.bytes.clone(),
                        channel,
                        checkpoint: entry.checkpoint,
                    },
                )
                .await?;
            room.record.published = room.record.published.saturating_add(1);
        }
        room.persist()?;
        self.observe_calls(room);
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

fn now_ns() -> u64 {
    tilo_clock::unix_now_ns()
}

fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}
