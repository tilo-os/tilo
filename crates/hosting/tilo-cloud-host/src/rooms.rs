//! The rooms this Cloud host holds for its person besides their home room:
//! the Tilo Community and their direct room with the Tilo team.
//!
//! Every alpha user is in the community by default. The host keeps them
//! there with the keeper every device runs ([`tilo_community::CommunityKeeper`]):
//! it joins on its own, retries while the community's host is out of reach,
//! opens the direct room with the team once admitted, and never joins again
//! a person who left. Each room is a [`HeldRoom`] (a `StatePeer` pinned to
//! the room's governing device) kept in a store root of its own under
//! `rooms/`, and carried by one relay session to the room's host.
use anyhow::{Context as _, Result, anyhow};
use ed25519_dalek::SigningKey;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};
use tilo_community::{CommunityHost, CommunityKeeper, Holding, StandingCommunity};
use tilo_desktop_model::RoomId;
use tilo_relay::coedit::Inbound;
use tilo_room_session::relay_room::{self, Entry, HeldRoom, HeldRoomRecord, HeldRoomStore};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::{mpsc, oneshot};

/// Directory below the state root holding one store root per held room.
const ROOMS: &str = "rooms";
/// The held room's record inside its store root.
const RECORD: &str = "held.json";
/// The person's settings on this host: their community membership choice.
const SETTINGS: &str = "settings.json";
/// How long a room waits before its session is dialed again.
const RECONNECT_AFTER: Duration = Duration::from_secs(2);
/// How often the rooms are looked after.
const TICK: Duration = Duration::from_millis(250);

/// One store root per held room: `rooms/<room>/held.json`, owner-only.
pub(crate) struct FileRooms {
    dir: PathBuf,
}

impl FileRooms {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            dir: root.join(ROOMS),
        }
    }

    fn room_root(&self, room: RoomId) -> PathBuf {
        self.dir.join(room.to_string())
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}

impl HeldRoomStore for FileRooms {
    fn held(&self) -> Result<Vec<HeldRoomRecord>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.to_string()),
        };
        let mut held = Vec::new();
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path().join(RECORD);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                // A root whose record never landed holds no room.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            held.push(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?);
        }
        Ok(held)
    }

    fn save(&self, record: &HeldRoomRecord) -> Result<(), String> {
        let root = self.room_root(record.room);
        private_dir(&root).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        private_fs::write_atomic(&root.join(RECORD), &bytes).map_err(|e| e.to_string())
    }

    fn remove(&self, room: RoomId) -> Result<(), String> {
        match std::fs::remove_dir_all(self.room_root(room)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// The Cloud host's device, as it joins rooms.
#[derive(Clone)]
pub(crate) struct Identity {
    pub(crate) cert: DeviceCert,
    pub(crate) key: SigningKey,
    pub(crate) revocations: RevocationList,
}

impl crate::Store {
    /// The host's device as it joins rooms: its enrolled certificate, key and
    /// the revocations its person published.
    ///
    /// # Errors
    /// The Store is not enrolled.
    pub(crate) fn rooms_identity(&self) -> Result<Identity> {
        let bound = self
            .state
            .bound
            .as_ref()
            .context("workspace not enrolled")?;
        Ok(Identity {
            cert: bound.authority.clone(),
            key: self.key(),
            revocations: crate::revocations(&bound.revocations)?,
        })
    }
}

/// How the host holds one room, for the person's devices to show.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HeldRoomStatus {
    pub room: String,
    pub workspace_id: String,
    /// `"community"` or `"direct"`.
    pub kind: &'static str,
    /// The room's governing device.
    pub governor: DeviceCert,
    /// Whether the room's host admitted this host yet.
    pub admitted: bool,
    /// The other person of a direct room, as a person id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_person: Option<String>,
}

/// The person's place in the community, as the keeper sees it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CommunityStatus {
    /// `"auto"` or `"left"`.
    pub membership: &'static str,
    /// `"no"`, `"held"` or `"admitted"`.
    pub holding: &'static str,
    pub room: Option<String>,
    /// Whether the community's host refused the configured invitation.
    pub refused: bool,
}

/// Every room held besides the home room, and the community.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RoomsStatus {
    pub rooms: Vec<HeldRoomStatus>,
    /// `None` when no community is configured.
    pub community: Option<CommunityStatus>,
}

enum Command {
    Status(oneshot::Sender<RoomsStatus>),
    Membership(
        settings_store::community::Membership,
        oneshot::Sender<Result<RoomsStatus, String>>,
    ),
}

/// Reaches the running rooms from the rest of the host.
#[derive(Clone)]
pub struct RoomsHandle {
    commands: mpsc::Sender<Command>,
}

impl RoomsHandle {
    /// Every held room and the community, now.
    ///
    /// # Errors
    /// The rooms stopped.
    pub async fn status(&self) -> Result<RoomsStatus> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Status(reply))
            .await
            .map_err(|_| anyhow!("Cloud rooms stopped"))?;
        answer.await.map_err(|_| anyhow!("Cloud rooms stopped"))
    }

    /// The person chose to leave the community, or to be in it again. The
    /// next look at the community acts on it.
    ///
    /// # Errors
    /// The choice cannot be kept, or the rooms stopped.
    pub async fn set_membership(
        &self,
        membership: settings_store::community::Membership,
    ) -> Result<RoomsStatus> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Membership(membership, reply))
            .await
            .map_err(|_| anyhow!("Cloud rooms stopped"))?;
        answer
            .await
            .map_err(|_| anyhow!("Cloud rooms stopped"))?
            .map_err(anyhow::Error::msg)
    }
}

enum Event {
    Delta {
        room: RoomId,
        author: [u8; 32],
        body: Vec<u8>,
        retained: bool,
    },
    Refused {
        room: RoomId,
        host: iroh::EndpointId,
        refusal: tilo_relay::transport::Refusal,
        reason: String,
    },
    Closed {
        room: RoomId,
        generation: u64,
    },
}

struct Session {
    generation: u64,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The held rooms and their sessions, driven by one task.
pub(crate) struct Rooms {
    identity: Identity,
    store: FileRooms,
    settings: PathBuf,
    community_config: PathBuf,
    endpoint: iroh::Endpoint,
    held: BTreeMap<RoomId, HeldRoom>,
    sessions: BTreeMap<RoomId, Session>,
    /// When a room without a session is dialed again.
    redial: BTreeMap<RoomId, Instant>,
    keeper: CommunityKeeper,
    events: (mpsc::Sender<Event>, mpsc::Receiver<Event>),
    generation: u64,
}

impl Rooms {
    /// The rooms held under `root` for `identity`, reading the community the
    /// release configures at `community_config`.
    ///
    /// # Errors
    /// A held room's record does not restore.
    pub(crate) fn open(
        root: &Path,
        identity: Identity,
        community_config: PathBuf,
        endpoint: iroh::Endpoint,
    ) -> Result<Self> {
        let store = FileRooms::new(root);
        let now = SystemTime::now();
        let mut held = BTreeMap::new();
        for record in store.held().map_err(anyhow::Error::msg)? {
            let room = HeldRoom::restore(record, &identity.cert, now)
                .map_err(anyhow::Error::msg)
                .context("a held room does not restore")?;
            held.insert(room.room(), room);
        }
        Ok(Self {
            identity,
            store,
            settings: root.join(SETTINGS),
            community_config,
            endpoint,
            held,
            sessions: BTreeMap::new(),
            redial: BTreeMap::new(),
            keeper: CommunityKeeper::default(),
            events: mpsc::channel(256),
            generation: 0,
        })
    }

    /// Run until `shutdown`, answering `handle`'s commands.
    pub(crate) fn start(
        self,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> (RoomsHandle, tokio::task::JoinHandle<()>) {
        let (commands, receiver) = mpsc::channel(16);
        let task = tokio::spawn(self.run(receiver, shutdown));
        (RoomsHandle { commands }, task)
    }

    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Command>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => break,
                command = commands.recv() => match command {
                    Some(command) => self.command(command),
                    None => break,
                },
                Some(event) = self.events.1.recv() => self.event(event),
                _ = tick.tick() => self.look_after(),
            }
        }
        self.sessions.clear();
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Status(reply) => {
                let _ = reply.send(self.status());
            }
            Command::Membership(membership, reply) => {
                let kept = settings_store::community::set_membership_at(&self.settings, membership)
                    .map_err(|error| error.to_string());
                if kept.is_ok() {
                    match membership {
                        settings_store::community::Membership::Left => self.keeper.left(),
                        settings_store::community::Membership::Auto => self.keeper.rejoin(),
                    }
                    self.look_after();
                }
                let _ = reply.send(kept.map(|()| self.status()));
            }
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Delta {
                room,
                author,
                body,
                retained,
            } => {
                let Some(held) = self.held.get_mut(&room) else {
                    return;
                };
                match held.receive(author, &body, retained, SystemTime::now()) {
                    Ok(true) => {
                        let saved = held.record().and_then(|record| self.store.save(&record));
                        if let Err(error) = saved {
                            tracing::warn!(%room, %error, "a held room could not be kept");
                        }
                    }
                    Ok(false) => {}
                    Err(error) => tracing::debug!(%room, %error, "held room message refused"),
                }
            }
            Event::Refused {
                room,
                host,
                refusal,
                reason,
            } => {
                tracing::info!(%room, ?refusal, %reason, "a room's host refused this host");
                if refusal == tilo_relay::transport::Refusal::NotMember {
                    self.keeper.note_refusal(|refused| refused == room, &host);
                }
            }
            Event::Closed { room, generation } => {
                if self
                    .sessions
                    .get(&room)
                    .is_some_and(|session| session.generation == generation)
                {
                    self.sessions.remove(&room);
                    self.redial.insert(room, Instant::now() + RECONNECT_AFTER);
                }
            }
        }
    }

    /// Keep the person where they want to be in the community, and every
    /// held room carried.
    fn look_after(&mut self) {
        let now = Instant::now();
        if self.keeper.is_due(now) {
            let community = self.community();
            let mut keeper = std::mem::take(&mut self.keeper);
            for note in keeper.tick(now, community, self) {
                tracing::info!(?note, "keeping the person in the Tilo Community");
            }
            self.keeper = keeper;
        }
        let due = self
            .held
            .keys()
            .filter(|room| !self.sessions.contains_key(room))
            .filter(|room| self.redial.get(room).is_none_or(|at| *at <= now))
            .copied()
            .collect::<Vec<_>>();
        for room in due {
            self.dial(room);
        }
    }

    fn community(&self) -> Result<StandingCommunity, String> {
        let config =
            tilo_community::load_from(&self.community_config).map_err(|error| error.to_string())?;
        tilo_community::standing(&config)
    }

    fn settings(&self) -> serde_json::Map<String, serde_json::Value> {
        settings_store::load_at(&self.settings)
    }

    /// Hold `room` from now on: keep it and carry it.
    fn hold(&mut self, room: HeldRoom) -> Result<(), String> {
        self.store.save(&room.record()?)?;
        let id = room.room();
        self.held.insert(id, room);
        self.sessions.remove(&id);
        self.redial.remove(&id);
        self.dial(id);
        Ok(())
    }

    /// Start one relay session for `room`, replacing any other.
    fn dial(&mut self, room: RoomId) {
        let Some(held) = self.held.get(&room).cloned() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let task = tokio::spawn(carry(
            self.endpoint.clone(),
            held,
            self.identity.clone(),
            self.events.0.clone(),
            generation,
        ));
        self.redial.remove(&room);
        self.sessions.insert(room, Session { generation, task });
    }

    fn status(&self) -> RoomsStatus {
        let now = SystemTime::now();
        let rooms = self
            .held
            .values()
            .map(|held| HeldRoomStatus {
                room: held.room().to_string(),
                workspace_id: held
                    .workspace()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                kind: match held.entry() {
                    Entry::Public { .. } => "community",
                    Entry::Direct { .. } => "direct",
                },
                governor: held.peer().authority().clone(),
                admitted: held.admits(self.identity.cert.device_public_key, now),
                peer_person: match held.entry() {
                    Entry::Direct { other } => Some(hex::encode(other)),
                    Entry::Public { .. } => None,
                },
            })
            .collect();
        let settings = self.settings();
        let community = self.community().ok().map(|community| {
            let invite = community.invite_hex.clone();
            CommunityStatus {
                membership: settings_store::community::membership_in(&settings).as_str(),
                holding: match self.holding(community.room) {
                    Holding::No => "no",
                    Holding::Held => "held",
                    Holding::Admitted => "admitted",
                },
                room: Some(community.room.to_string()),
                refused: settings_store::community::refused_invite_in(&settings)
                    == Some(invite.as_str()),
            }
        });
        RoomsStatus { rooms, community }
    }
}

impl CommunityHost for Rooms {
    fn has_identity(&self) -> bool {
        true
    }

    fn membership(&self) -> settings_store::community::Membership {
        settings_store::community::membership_in(&self.settings())
    }

    fn refused_invite(&self) -> Option<String> {
        settings_store::community::refused_invite_in(&self.settings()).map(str::to_string)
    }

    fn holding(&self, room: RoomId) -> Holding {
        match self.held.get(&room) {
            None => Holding::No,
            Some(held) if held.admits(self.identity.cert.device_public_key, SystemTime::now()) => {
                Holding::Admitted
            }
            Some(_) => Holding::Held,
        }
    }

    fn join(&mut self, community: &StandingCommunity) -> Result<(), String> {
        let room =
            HeldRoom::enter_public(&community.invite, &self.identity.cert, SystemTime::now())?;
        self.hold(room)
    }

    fn redeem(&mut self, community: &StandingCommunity) {
        // A fresh session redeems the invitation again before it subscribes.
        self.dial(community.room);
    }

    fn open_team_room(&mut self) -> Result<(), String> {
        let config =
            tilo_community::load_from(&self.community_config).map_err(|error| error.to_string())?;
        let team = config.team.ok_or_else(|| {
            format!(
                "no Tilo team principal is configured in {}",
                config.source.display()
            )
        })?;
        let hosts = config
            .invite
            .as_deref()
            .and_then(|invite| neighbour_wire::invite::validate_invite_token(invite).ok())
            .map(|parsed| parsed.host_addrs)
            .unwrap_or_default();
        let room = HeldRoom::open_direct(
            team.user_public_key,
            hosts,
            &self.identity.cert,
            &self.identity.key,
            SystemTime::now(),
        )?;
        if self.held.contains_key(&room.room()) {
            return Ok(());
        }
        self.hold(room)
    }

    fn remember_refusal(&mut self, invite_hex: &str) -> Result<(), String> {
        settings_store::community::set_refused_invite_at(&self.settings, Some(invite_hex))
            .map_err(|error| error.to_string())
    }

    fn leave(&mut self, room: RoomId) -> Result<(), String> {
        self.sessions.remove(&room);
        self.redial.remove(&room);
        self.store.remove(room)?;
        self.held.remove(&room);
        Ok(())
    }
}

/// Carry `room` over one relay session until it ends, handing everything
/// that arrives to the rooms task.
async fn carry(
    endpoint: iroh::Endpoint,
    room: HeldRoom,
    identity: Identity,
    events: mpsc::Sender<Event>,
    generation: u64,
) {
    let id = room.room();
    let (session, host) = match relay_room::connect(
        endpoint,
        &room,
        &identity.cert,
        &identity.key,
        &identity.revocations,
    )
    .await
    {
        Ok(connected) => connected,
        Err(error) => {
            tracing::debug!(room = %id, %error, "a held room's host could not be reached");
            let _ = events
                .send(Event::Closed {
                    room: id,
                    generation,
                })
                .await;
            return;
        }
    };
    if let Err(error) = room.announce(&session, &identity.cert, &identity.revocations) {
        tracing::debug!(room = %id, %error, "a held room's announcements were not sent");
    }
    // What arrives before `Ready` is the host's retained state.
    let mut retained = true;
    while let Some(inbound) = session.recv().await {
        let event = match inbound {
            Inbound::Delta { author, data } => Event::Delta {
                room: id,
                author,
                body: data,
                retained,
            },
            Inbound::Ready { .. } => {
                retained = false;
                continue;
            }
            Inbound::Refused { refusal, reason } => Event::Refused {
                room: id,
                host,
                refusal,
                reason,
            },
            Inbound::Presence { .. } | Inbound::RemoteAction { .. } | Inbound::Replayed { .. } => {
                continue;
            }
        };
        if events.send(event).await.is_err() {
            return;
        }
    }
    let _ = events
        .send(Event::Closed {
            room: id,
            generation,
        })
        .await;
}

#[cfg(test)]
mod tests;
