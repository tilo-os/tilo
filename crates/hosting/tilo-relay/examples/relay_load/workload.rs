//! The simulated collaborators and the minute of work each one repeats.
//!
//! Everything a collaborator publishes takes the production path and the
//! production encoding: edits are Automerge changes signed and admitted by a
//! `StatePeer`, classified by the host retention rule and published as
//! retained deltas; presence is the shell's own `NeighbourMsg`, wrapped by
//! `RelayCoeditSession::publish_presence`. The input is deterministic: the
//! same text, the same backspaces, the same pointer path, every run.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, anyhow};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, transaction::Transactable};
use collab_types::{PeerPresence, PeerPresencePayload};
use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use iroh::endpoint::{Endpoint, presets};
use tokio::task::JoinHandle;

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_room};
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use crate::relay_process::Setup;
use tilo_lock::MutexRecover as _;

/// The document every room edits together.
const APP: &str = "so.tilo.word-processor";
const RECORD: &str = "tab:0000000000000001";
/// Characters in the document before anyone types (~10 kB).
const DOCUMENT_CHARS: usize = 10_000;
/// What collaborators type, character by character, over and over.
const TEXT: &str = "Collaboration lives below applications rather than inside them, so it \
                    has to work in the ebook reader, the file manager and the terminal. ";
/// Every this many keystrokes is a backspace.
const BACKSPACE_EVERY: u64 = 16;

/// The minute: 30 edits at 5 per second, a pause, 4 seconds of pointer
/// motion at the shell's 30 Hz publish gap, then idle.
const EDITS_PER_MINUTE: u32 = 30;
const EDIT_GAP: Duration = Duration::from_millis(200);
const POINTER_START: Duration = Duration::from_secs(8);
const POINTER_MOVES_PER_MINUTE: u32 = 120;
const POINTER_GAP: Duration = Duration::from_millis(33);
const MINUTE: Duration = Duration::from_secs(60);
/// Spreads the collaborators' minutes over the minute.
const STAGGER_MS: u64 = 7_919;
/// How long the last edits get to arrive before the sessions close.
const GRACE: Duration = Duration::from_secs(5);
/// Sessions dialled at once.
const DIAL_BATCH: usize = 32;
/// Bounds a subscription that never gets ready: only a hang reaches it.
const READY: Duration = Duration::from_secs(120);
/// A published presence sample nobody received by now was superseded by a
/// later one on the relay (coalesced), and is forgotten.
const PRESENCE_FORGET: Duration = Duration::from_secs(2);
/// Unreceived presence samples kept before the forgotten ones are swept.
const PRESENCE_SWEEP: usize = 4_096;
const COLOUR: &str = "teal";

struct Device {
    sk: SigningKey,
    cert: DeviceCert,
}

impl Device {
    const fn public_key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

struct Room {
    workspace_id: u64,
    uuid: String,
    members: Vec<Device>,
    /// The room as its authority seeded it; each member starts from a copy.
    seed: StatePeer,
}

/// An edit in flight: when it was published and how many members still
/// have to receive it.
struct Pending {
    sent: Instant,
    remaining: usize,
}

/// The presence whose delivery is timed: a pointer move and a caret.
#[derive(Clone, Copy)]
enum Timed {
    Pointer,
    Caret,
}

/// A presence sample in flight: when it was published, what it is and how
/// many members still have to receive it.
struct PendingPresence {
    sent: Instant,
    timed: Timed,
    remaining: usize,
}

/// Every room and collaborator of a run, and what they observed.
pub struct Fleet {
    user_public_key: [u8; 32],
    relay: Device,
    rooms: Vec<Room>,
    /// Latencies are kept only while this is set.
    pub measuring: AtomicBool,
    pub edits: AtomicU64,
    pub presence: AtomicU64,
    pending: Mutex<HashMap<blake3::Hash, Pending>>,
    latencies: Mutex<Vec<f64>>,
    presence_pending: Mutex<HashMap<blake3::Hash, PendingPresence>>,
    pointer_latencies: Mutex<Vec<f64>>,
    caret_latencies: Mutex<Vec<f64>>,
}

impl Fleet {
    /// Mint a user, the relay's device and `collaborators` devices in rooms
    /// of `room_size`, and seed each room's document.
    pub fn enrol(collaborators: usize, room_size: usize) -> anyhow::Result<Arc<Self>> {
        let (user_sk, user) = UserIdentity::generate("Hosting cost bench", 1);
        let enrol = |label: String, capabilities: DeviceCapabilities| -> anyhow::Result<Device> {
            let (sk, device) = DeviceIdentity::generate(label, 100);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: device.public_key,
                device_label: device.label,
                capabilities,
                issued_at_ns: 200,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &user_sk).map_err(|error| anyhow!("{error}"))?;
            Ok(Device { sk, cert })
        };
        let relay = enrol("relay".into(), DeviceCapabilities::host())?;
        let mut rooms = Vec::new();
        let mut enrolled = 0;
        while enrolled < collaborators {
            let size = room_size.min(collaborators.saturating_sub(enrolled)).max(2);
            let index = rooms.len();
            let members = (0..size)
                .map(|member| {
                    let capabilities = if member == 0 {
                        DeviceCapabilities::primary()
                    } else {
                        DeviceCapabilities::member()
                    };
                    enrol(format!("collaborator {index}.{member}"), capabilities)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            enrolled = enrolled.saturating_add(size);
            rooms.push(Room::seed(index, members)?);
        }
        Ok(Arc::new(Self {
            user_public_key: user.public_key,
            relay,
            rooms,
            measuring: AtomicBool::new(false),
            edits: AtomicU64::new(0),
            presence: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            latencies: Mutex::new(Vec::new()),
            presence_pending: Mutex::new(HashMap::new()),
            pointer_latencies: Mutex::new(Vec::new()),
            caret_latencies: Mutex::new(Vec::new()),
        }))
    }

    pub fn rooms(&self) -> usize {
        self.rooms.len()
    }

    /// The relay process's identity and rooms.
    pub fn relay_setup(&self, state: &Path) -> Setup {
        Setup {
            state_root: state.to_path_buf(),
            user_public_key: self.user_public_key,
            relay_secret_key: self.relay.sk.to_bytes(),
            relay_cert: self.relay.cert.clone(),
            rooms: self
                .rooms
                .iter()
                .map(|room| {
                    (
                        room.workspace_id,
                        room.members.iter().map(Device::public_key).collect(),
                    )
                })
                .collect(),
        }
    }

    /// Dial the relay as every collaborator and wait until each is
    /// subscribed; each session's receiver then runs until it closes.
    pub async fn connect(
        self: &Arc<Self>,
        relay: &EndpointAddr,
        relay_device: [u8; 32],
    ) -> anyhow::Result<Vec<Collaborator>> {
        let mut seats = Vec::new();
        for (room_index, room) in self.rooms.iter().enumerate() {
            for member in 0..room.members.len() {
                seats.push((room_index, member));
            }
        }
        let mut collaborators = Vec::with_capacity(seats.len());
        for batch in seats.chunks(DIAL_BATCH) {
            let dials: Vec<_> = batch
                .iter()
                .map(|&(room, member)| {
                    let fleet = self.clone();
                    let relay = relay.clone();
                    tokio::spawn(async move { fleet.join(room, member, relay, relay_device).await })
                })
                .collect();
            for dial in dials {
                collaborators.push(dial.await??);
            }
        }
        for (index, collaborator) in collaborators.iter_mut().enumerate() {
            collaborator.index = index;
        }
        Ok(collaborators)
    }

    async fn join(
        self: Arc<Self>,
        room_index: usize,
        member: usize,
        relay: EndpointAddr,
        relay_device: [u8; 32],
    ) -> anyhow::Result<Collaborator> {
        let (workspace_id, cert, sk) = {
            let room = self.rooms.get(room_index).context("room")?;
            let device = room.members.get(member).context("member")?;
            (room.workspace_id, device.cert.clone(), device.sk.clone())
        };
        let device_public_key = cert.device_public_key;
        let endpoint = Endpoint::builder(presets::Minimal).bind().await?;
        let client = RelayClient::connect_v2_to_enrolled_home_peer(
            endpoint,
            relay,
            self.user_public_key,
            relay_device,
            cert,
            sk,
            RevocationList::new(),
        )
        .await?;
        let session = Arc::new(RelayCoeditSession::join(
            client,
            workspace_id,
            device_public_key,
        )?);
        tokio::time::timeout(READY, async {
            loop {
                match session.recv().await {
                    Some(Inbound::Ready { .. }) => return Ok(()),
                    Some(_) => {}
                    None => return Err(anyhow!("the relay closed a subscription")),
                }
            }
        })
        .await
        .context("a subscription never got ready")??;
        tokio::spawn(receive(self.clone(), session.clone()));
        let room = self.rooms.get(room_index).context("room")?;
        let device = room.members.get(member).context("member")?;
        Collaborator::new(self.clone(), room, device, session)
    }

    /// Note an edit about to be published to `recipients` other members.
    fn sent(&self, body: &[u8], recipients: usize) {
        self.pending.lock_recover().insert(
            blake3::hash(body),
            Pending {
                sent: Instant::now(),
                remaining: recipients,
            },
        );
    }

    /// Note that a member received the edit `body`.
    fn received(&self, body: &[u8]) {
        let now = Instant::now();
        let sent = {
            let mut pending = self.pending.lock_recover();
            let key = blake3::hash(body);
            let Some(entry) = pending.get_mut(&key) else {
                return;
            };
            entry.remaining = entry.remaining.saturating_sub(1);
            let sent = entry.sent;
            if entry.remaining == 0 {
                pending.remove(&key);
            }
            sent
        };
        if self.measuring.load(Ordering::SeqCst) {
            self.latencies
                .lock_recover()
                .push(now.duration_since(sent).as_secs_f64() * 1_000.0);
        }
    }

    /// Note a timed presence sample about to be published to `recipients`
    /// other members.
    fn presence_sent(&self, body: &[u8], timed: Timed, recipients: usize) {
        let now = Instant::now();
        let mut pending = self.presence_pending.lock_recover();
        if pending.len() >= PRESENCE_SWEEP {
            pending.retain(|_, sample| now.duration_since(sample.sent) < PRESENCE_FORGET);
        }
        pending.insert(
            blake3::hash(body),
            PendingPresence {
                sent: now,
                timed,
                remaining: recipients,
            },
        );
    }

    /// Note that a member received the presence `body`. Only samples the
    /// relay forwarded arrive; one it coalesced away never does.
    fn presence_received(&self, body: &[u8]) {
        let now = Instant::now();
        let (sent, timed) = {
            let mut pending = self.presence_pending.lock_recover();
            let key = blake3::hash(body);
            let Some(entry) = pending.get_mut(&key) else {
                return;
            };
            entry.remaining = entry.remaining.saturating_sub(1);
            let found = (entry.sent, entry.timed);
            if entry.remaining == 0 {
                pending.remove(&key);
            }
            found
        };
        if self.measuring.load(Ordering::SeqCst) {
            let latencies = match timed {
                Timed::Pointer => &self.pointer_latencies,
                Timed::Caret => &self.caret_latencies,
            };
            latencies
                .lock_recover()
                .push(now.duration_since(sent).as_secs_f64() * 1_000.0);
        }
    }

    /// The pointer and caret delivery latencies measured so far, in
    /// milliseconds: publish to receipt by each other member of the room.
    pub fn take_presence_latencies(&self) -> (Vec<f64>, Vec<f64>) {
        let take = |latencies: &Mutex<Vec<f64>>| std::mem::take(&mut *latencies.lock_recover());
        (take(&self.pointer_latencies), take(&self.caret_latencies))
    }

    /// The fan-out latencies measured so far, in milliseconds.
    pub fn take_latencies(&self) -> Vec<f64> {
        std::mem::take(&mut *self.latencies.lock_recover())
    }

    /// Deliveries of published edits that never arrived.
    pub fn undelivered(&self) -> usize {
        self.pending
            .lock_recover()
            .values()
            .map(|pending| pending.remaining)
            .sum()
    }
}

impl Room {
    fn seed(index: usize, members: Vec<Device>) -> anyhow::Result<Self> {
        let uuid = format!("5e11f00d-7110-4625-8000-{index:012x}");
        let authority = members.first().context("a room has members")?;
        let certificates: Vec<DeviceCert> =
            members.iter().map(|device| device.cert.clone()).collect();
        let now = SystemTime::now();
        let mut seed = StatePeer::new(
            &uuid,
            authority.cert.clone(),
            certificates.clone(),
            RevocationList::new(),
            now,
        )
        .map_err(|error| anyhow!("{error}"))?;
        let mut document = AutoCommit::new();
        let body = document.put_object(ROOT, "body", ObjType::Text)?;
        document.splice_text(&body, 0, 0, &"x".repeat(DOCUMENT_CHARS))?;
        document.commit();
        seed.install_seed(
            SeedState {
                room_changes: seed_room(&uuid, &authority.cert, &certificates)
                    .map_err(|error| anyhow!("{error}"))?,
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "text".into(),
                    bytes: document.save(),
                }],
            },
            &authority.sk,
            now,
        )
        .map_err(|error| anyhow!("{error}"))?;
        Ok(Self {
            workspace_id: 592_000_u64.saturating_add(u64::try_from(index)?),
            uuid,
            members,
            seed,
        })
    }
}

/// Drain one session, timing every edit, pointer move and caret another
/// member published.
async fn receive(fleet: Arc<Fleet>, session: Arc<RelayCoeditSession>) {
    while let Some(inbound) = session.recv().await {
        match inbound {
            Inbound::Delta { data, .. } => fleet.received(&data),
            Inbound::Presence { payload, .. } => fleet.presence_received(&payload),
            _ => {}
        }
    }
}

/// One connected collaborator: a device, its replica of the room and its
/// session.
pub struct Collaborator {
    pub index: usize,
    pub session: Arc<RelayCoeditSession>,
    fleet: Arc<Fleet>,
    sk: SigningKey,
    peer: StatePeer,
    doc: AutoCommit,
    body: ObjId,
    cursor: usize,
    keystrokes: u64,
    recipients: usize,
    room: String,
    handle: u64,
    user: String,
    caret_key: Vec<u8>,
    pointer_key: String,
}

impl Collaborator {
    fn new(
        fleet: Arc<Fleet>,
        room: &Room,
        device: &Device,
        session: Arc<RelayCoeditSession>,
    ) -> anyhow::Result<Self> {
        let peer = room.seed.clone();
        let mut doc = AutoCommit::load(&peer.document(APP, RECORD).context("seeded document")?)?;
        doc.set_actor(automerge::ActorId::from(device.public_key().to_vec()));
        let (_, body) = doc.get(ROOT, "body")?.context("document body")?;
        // Members type in different places, as people writing different
        // paragraphs of one document would.
        let place = room
            .members
            .iter()
            .position(|member| member.public_key() == device.public_key())
            .unwrap_or(0);
        let cursor = DOCUMENT_CHARS
            .saturating_mul(place.saturating_add(1))
            .checked_div(room.members.len().saturating_add(1))
            .unwrap_or(0);
        Ok(Self {
            index: 0,
            session,
            sk: device.sk.clone(),
            peer,
            doc,
            body,
            cursor,
            keystrokes: 0,
            recipients: room.members.len().saturating_sub(1),
            room: room.uuid.clone(),
            handle: neighbour_wire::peer_handle(&device.public_key()),
            user: hex::encode(fleet.user_public_key),
            // The toolkit's key for a text field and the shell's for a
            // pointer over a tile, serialised as they are sent.
            caret_key: serde_json::to_vec(&ui::PresenceKey::Widget {
                path: vec![ui::PathSegment::Field("body".into())],
            })?,
            pointer_key: serde_json::to_string(&ui::PresenceKey::tile_pointer(&room.uuid, 1))?,
            fleet,
        })
    }

    /// One keystroke: the edit, then the toolkit's typing and caret presence.
    fn type_one(&mut self) -> anyhow::Result<()> {
        let heads = self.doc.get_heads();
        let keystroke = self.keystrokes;
        self.keystrokes = self.keystrokes.saturating_add(1);
        let start = self.cursor;
        let backspace =
            keystroke.checked_rem(BACKSPACE_EVERY) == Some(BACKSPACE_EVERY.saturating_sub(1));
        if backspace && self.cursor > 0 {
            self.cursor = self.cursor.saturating_sub(1);
            self.doc.splice_text(&self.body, self.cursor, 1, "")?;
        } else {
            let at = usize::try_from(keystroke)?
                .checked_rem(TEXT.len())
                .unwrap_or(0);
            let character = TEXT.get(at..at.saturating_add(1)).unwrap_or(" ");
            self.doc
                .splice_text(&self.body, self.cursor, 0, character)?;
            self.cursor = self.cursor.saturating_add(1);
        }
        self.doc.commit();
        let change = self.doc.save_after(&heads);
        let message = self
            .peer
            .publish_incremental(APP, RECORD, change, &self.sk, SystemTime::now())
            .map_err(|error| anyhow!("publish an edit: {error}"))?;
        let (channel, _) =
            neighbour_wire::retention(&message, false).context("an edit is retained history")?;
        let body = postcard::to_allocvec(&message)?;
        self.fleet.sent(&body, self.recipients);
        self.session.publish_retained_delta(channel, body)?;
        self.fleet.edits.fetch_add(1, Ordering::Relaxed);
        let (low, high) = (start.min(self.cursor), start.max(self.cursor));
        self.app_presence(PeerPresencePayload::Typing {
            start: u64::try_from(low)?,
            end: u64::try_from(high)?,
        })?;
        self.app_presence(PeerPresencePayload::Caret {
            pos: u64::try_from(self.cursor)?,
        })
    }

    fn app_presence(&self, payload: PeerPresencePayload) -> anyhow::Result<()> {
        let timed = matches!(payload, PeerPresencePayload::Caret { .. }).then_some(Timed::Caret);
        self.presence(
            &NeighbourMsg::AppPresenceV2 {
                peer: self.handle,
                app_id: APP.into(),
                cluster_id: self.room.clone(),
                presence: PeerPresence {
                    key: self.caret_key.clone(),
                    payload,
                },
                colour: Some(COLOUR.into()),
            },
            timed,
        )
    }

    /// One pointer move along a fixed path.
    fn point(&self, step: u32) -> anyhow::Result<()> {
        self.presence(
            &NeighbourMsg::PresenceMoved {
                peer: self.handle,
                user: self.user.clone(),
                key: self.pointer_key.clone(),
                point: (
                    f64::from(step).mul_add(3.0, 200.0),
                    f64::from(step.checked_rem(20).unwrap_or(0)).mul_add(2.0, 300.0),
                ),
                ts: tilo_clock::unix_now_ms(),
                colour: Some(COLOUR.into()),
            },
            Some(Timed::Pointer),
        )
    }

    fn presence(&self, message: &NeighbourMsg, timed: Option<Timed>) -> anyhow::Result<()> {
        let body = postcard::to_allocvec(message)?;
        if let Some(timed) = timed {
            self.fleet.presence_sent(&body, timed, self.recipients);
        }
        self.session.publish_presence(body)?;
        self.fleet.presence.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Run every collaborator's minute from `start` until `end`; each task ends
/// a grace period after `end`, when the last edits had time to arrive.
pub fn play(
    collaborators: Vec<Collaborator>,
    start: Instant,
    end: Instant,
) -> Vec<JoinHandle<anyhow::Result<()>>> {
    collaborators
        .into_iter()
        .map(|collaborator| tokio::spawn(act(collaborator, start, end)))
        .collect()
}

async fn act(mut collaborator: Collaborator, start: Instant, end: Instant) -> anyhow::Result<()> {
    let offset = u64::try_from(collaborator.index)?
        .wrapping_mul(STAGGER_MS)
        .checked_rem(60_000)
        .unwrap_or(0);
    let mut minute = start
        .checked_add(Duration::from_millis(offset))
        .context("clock")?;
    'run: while minute < end {
        for edit in 0..EDITS_PER_MINUTE {
            let due = later(minute, EDIT_GAP, edit)?;
            if due >= end {
                break 'run;
            }
            tokio::time::sleep_until(due.into()).await;
            collaborator.type_one()?;
        }
        for step in 0..POINTER_MOVES_PER_MINUTE {
            let due = later(later(minute, POINTER_START, 1)?, POINTER_GAP, step)?;
            if due >= end {
                break 'run;
            }
            tokio::time::sleep_until(due.into()).await;
            collaborator.point(step)?;
        }
        minute = minute.checked_add(MINUTE).context("clock")?;
    }
    tokio::time::sleep_until(end.checked_add(GRACE).context("clock")?.into()).await;
    Ok(())
}

/// `base + gap * n`.
fn later(base: Instant, gap: Duration, n: u32) -> anyhow::Result<Instant> {
    base.checked_add(gap.checked_mul(n).context("duration")?)
        .context("clock")
}
