//! The simulated members and the minute of work each one repeats.
//!
//! Everything a member publishes takes the production path and the
//! production encoding: each member runs the same link a device's daemon runs
//! (`tilo_server_client::link` and `DirectSync`), edits are Automerge changes
//! signed and admitted by the member's `StatePeer` and published on the link,
//! and presence is a shell-shaped payload carried as a live presence frame.
//! The input is deterministic: the same text, the same backspaces, the same
//! pointer path, every run.

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
use neighbour_wire::NeighbourMsg;
use tokio::task::JoinHandle;

use tilo_peer_state::direct::{DirectSync, Reader};
use tilo_peer_state::transport::Transport;
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_server_client::ServerAddress;
use tilo_server_client::link::{Dialer, Link, Purpose};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use crate::server_process::Setup;
use collab_types::WorkspaceId;
use tilo_lock::MutexRecover as _;

/// The document every workspace edits together.
const APP: &str = "so.tilo.word-processor";
const RECORD: &str = "tab:0000000000000001";
/// Characters in the document before anyone types (~10 kB).
const DOCUMENT_CHARS: usize = 10_000;
/// What members type, character by character, over and over.
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
/// Spreads the members' minutes over the minute.
const STAGGER_MS: u64 = 7_919;
/// How long the last edits get to arrive before the sessions close.
const GRACE: Duration = Duration::from_secs(5);
/// Sessions dialled at once.
const DIAL_BATCH: usize = 32;
/// Bounds a subscription that never gets ready: only a hang reaches it.
const READY: Duration = Duration::from_secs(120);
/// A published presence sample nobody received by now was superseded by a
/// later one on the server (coalesced), and is forgotten.
const PRESENCE_FORGET: Duration = Duration::from_secs(2);
/// Unreceived presence samples kept before the forgotten ones are swept.
const PRESENCE_SWEEP: usize = 4_096;
const COLOUR: &str = "teal";

/// An app's typing and caret report, as the shell publishes it.
#[derive(serde::Serialize)]
struct AppPresence<'a> {
    peer: u64,
    app_id: &'a str,
    cluster_id: &'a str,
    presence: PeerPresence,
    colour: Option<&'a str>,
}

/// One pointer move, as the shell publishes it.
#[derive(serde::Serialize)]
struct PointerMove<'a> {
    peer: u64,
    user: &'a str,
    key: &'a str,
    point: (f64, f64),
    ts: u64,
    colour: Option<&'a str>,
}

struct Device {
    sk: SigningKey,
    cert: DeviceCert,
}

impl Device {
    const fn public_key(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

struct Workspace {
    id: WorkspaceId,
    uuid: String,
    members: Vec<Device>,
    /// The workspace as its authority seeded it; each member starts from a copy.
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

/// Every workspace and member of a run, and what they observed.
pub struct Fleet {
    user_public_key: [u8; 32],
    server: Device,
    workspaces: Vec<Workspace>,
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
    /// Mint a user, the server's device and `members` devices in workspaces
    /// of `workspace_size`, and seed each workspace's document.
    pub fn enrol(members: usize, workspace_size: usize) -> anyhow::Result<Arc<Self>> {
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
                issuer: None,
            };
            sign_device_cert(&mut cert, &user_sk).map_err(|error| anyhow!("{error}"))?;
            Ok(Device { sk, cert })
        };
        let server = enrol("server".into(), DeviceCapabilities::member())?;
        let mut workspaces = Vec::new();
        let mut enrolled = 0;
        while enrolled < members {
            let size = workspace_size.min(members.saturating_sub(enrolled)).max(2);
            let index = workspaces.len();
            let members = (0..size)
                .map(|member| {
                    let capabilities = if member == 0 {
                        DeviceCapabilities::primary()
                    } else {
                        DeviceCapabilities::member()
                    };
                    enrol(format!("member {index}.{member}"), capabilities)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            enrolled = enrolled.saturating_add(size);
            workspaces.push(Workspace::seed(index, members)?);
        }
        Ok(Arc::new(Self {
            user_public_key: user.public_key,
            server,
            workspaces,
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

    pub fn workspaces(&self) -> usize {
        self.workspaces.len()
    }

    /// The server process's identity and workspaces.
    pub fn server_setup(&self, state: &Path) -> Setup {
        Setup {
            state_root: state.to_path_buf(),
            user_public_key: self.user_public_key,
            server_secret_key: self.server.sk.to_bytes(),
            server_cert: self.server.cert.clone(),
            workspaces: self
                .workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace.id,
                        workspace
                            .members
                            .iter()
                            .map(|device| (device.cert.clone(), device.sk.to_bytes()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    /// Dial the server as every member and wait until each is
    /// subscribed; each session's receiver then runs until it closes.
    pub async fn connect(
        self: &Arc<Self>,
        server: &EndpointAddr,
        server_device: [u8; 32],
    ) -> anyhow::Result<Vec<Member>> {
        let mut joins = Vec::new();
        for (workspace_index, workspace) in self.workspaces.iter().enumerate() {
            for member in 0..workspace.members.len() {
                joins.push((workspace_index, member));
            }
        }
        let mut members = Vec::with_capacity(joins.len());
        for batch in joins.chunks(DIAL_BATCH) {
            let dials: Vec<_> = batch
                .iter()
                .map(|&(workspace, member)| {
                    let fleet = self.clone();
                    let server = server.clone();
                    tokio::spawn(async move {
                        fleet.join(workspace, member, server, server_device).await
                    })
                })
                .collect();
            for dial in dials {
                members.push(dial.await??);
            }
        }
        for (index, member) in members.iter_mut().enumerate() {
            member.index = index;
        }
        Ok(members)
    }

    async fn join(
        self: Arc<Self>,
        workspace_index: usize,
        member: usize,
        server: EndpointAddr,
        server_device: [u8; 32],
    ) -> anyhow::Result<Member> {
        let (cert, sk, seed) = {
            let workspace = self.workspaces.get(workspace_index).context("workspace")?;
            let device = workspace.members.get(member).context("member")?;
            (
                device.cert.clone(),
                device.sk.clone(),
                workspace.seed.clone(),
            )
        };
        // One endpoint under the device's key, as a device's daemon runs one: its links are that
        // device's, and the host admits it by the handshake on each link's first stream.
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .secret_key(iroh::SecretKey::from_bytes(&sk.to_bytes()))
            .bind()
            .await?;
        let dialer = Arc::new(Dialer::from_endpoint(
            ServerAddress::at(server.clone()),
            endpoint,
        ));
        let link = Arc::new(
            dialer
                .link(Purpose::Records)
                .await
                .map_err(|error| anyhow!("{error}"))?,
        );
        link.prove_member(&cert, &sk, &self.server.cert)
            .await
            .map_err(|error| anyhow!("{error}"))?;
        let sync = DirectSync::new(seed, sk.clone());
        let transport: Arc<dyn Transport> = link.clone();
        let running_session = sync
            .dial_over(transport, server_device)
            .await
            .map_err(|refusal| anyhow!("{refusal}"))?;
        let reader = sync.reader();
        let running = sync.clone();
        let task = tokio::spawn(async move {
            let _ = running_session.run(&running).await;
        });
        let mut caught_up = sync.caught_up();
        tokio::time::timeout(READY, caught_up.wait_for(|caught_up| *caught_up))
            .await
            .context("a link never caught up")?
            .map_err(|error| anyhow!("{error}"))?;
        tokio::spawn(receive(self.clone(), reader));
        let workspace = self.workspaces.get(workspace_index).context("workspace")?;
        let device = workspace.members.get(member).context("member")?;
        Member::new(self.clone(), workspace, device, sync, link, dialer, task)
    }

    /// Note an edit about to be published to `recipients` other members.
    fn sent(&self, message: &NeighbourMsg, recipients: usize) {
        let body = postcard::to_allocvec(message).unwrap_or_default();
        self.pending.lock_recover().insert(
            blake3::hash(&body),
            Pending {
                sent: Instant::now(),
                remaining: recipients,
            },
        );
    }

    /// Note that a member received the edit `message`.
    fn received(&self, message: &NeighbourMsg) {
        let now = Instant::now();
        let body = postcard::to_allocvec(message).unwrap_or_default();
        let sent = {
            let mut pending = self.pending.lock_recover();
            let key = blake3::hash(&body);
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
    /// server forwarded arrive; one it coalesced away never does.
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
    /// milliseconds: publish to receipt by each other member of the workspace.
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

impl Workspace {
    fn seed(index: usize, members: Vec<Device>) -> anyhow::Result<Self> {
        let uuid = format!("5e11f00d-7110-4625-8000-{index:012x}");
        let authority = members.first().context("a workspace has members")?;
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
                workspace_changes: seed_workspace(&uuid, &authority.cert, &certificates)
                    .map_err(|error| anyhow!("{error}"))?,
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    document_id: RECORD.into(),
                    kind: "text".into(),
                    bytes: document.save(),
                }],
            },
            &authority.sk,
            now,
        )
        .map_err(|error| anyhow!("{error}"))?;
        Ok(Self {
            id: WorkspaceId::from_u128(592_000_u128.saturating_add(u128::try_from(index)?)),
            uuid,
            members,
            seed,
        })
    }
}

/// Drain one link, timing every edit, pointer move and caret another member published.
async fn receive(fleet: Arc<Fleet>, mut reader: Reader) {
    while let Some(heard) = reader.recv().await {
        match &heard.message {
            NeighbourMsg::AppDocument { .. } => fleet.received(&heard.message),
            NeighbourMsg::LivePresence { frame, .. } => fleet.presence_received(frame),
            _ => {}
        }
    }
}

/// One connected member: a device, its copy of the workspace and its link.
pub struct Member {
    pub index: usize,
    /// The member's link, for its transport counters.
    pub link: Arc<Link>,
    fleet: Arc<Fleet>,
    sk: SigningKey,
    sync: DirectSync,
    doc: AutoCommit,
    body: ObjId,
    cursor: usize,
    keystrokes: u64,
    recipients: usize,
    workspace: String,
    handle: u64,
    user: String,
    caret_key: Vec<u8>,
    pointer_key: String,
    /// Kept for the endpoint its link runs from.
    _dialer: Arc<Dialer>,
    _running: tokio::task::JoinHandle<()>,
}

impl Member {
    fn new(
        fleet: Arc<Fleet>,
        workspace: &Workspace,
        device: &Device,
        sync: DirectSync,
        link: Arc<Link>,
        dialer: Arc<Dialer>,
        running: tokio::task::JoinHandle<()>,
    ) -> anyhow::Result<Self> {
        let mut doc = AutoCommit::load(
            &workspace
                .seed
                .document(APP, RECORD)
                .context("seeded document")?,
        )?;
        doc.set_actor(automerge::ActorId::from(device.public_key().to_vec()));
        let (_, body) = doc.get(ROOT, "body")?.context("document body")?;
        // Members type in different places, as people writing different
        // paragraphs of one document would.
        let place = workspace
            .members
            .iter()
            .position(|member| member.public_key() == device.public_key())
            .unwrap_or(0);
        let cursor = DOCUMENT_CHARS
            .saturating_mul(place.saturating_add(1))
            .checked_div(workspace.members.len().saturating_add(1))
            .unwrap_or(0);
        let user = hex::encode(fleet.user_public_key);
        Ok(Self {
            index: 0,
            link,
            fleet,
            sk: device.sk.clone(),
            sync,
            doc,
            body,
            cursor,
            keystrokes: 0,
            recipients: workspace.members.len().saturating_sub(1),
            workspace: workspace.uuid.clone(),
            handle: neighbour_wire::peer_handle(&device.public_key()),
            user,
            // The toolkit's key for a text field and the shell's for a
            // pointer over a tile, serialised as they are sent.
            caret_key: serde_json::to_vec(&tilo_ui_base::PresenceKey::Widget {
                path: vec![tilo_ui_base::PathSegment::Field("body".into())],
            })?,
            pointer_key: serde_json::to_string(&tilo_ui_base::PresenceKey::tile_pointer(&workspace.uuid, 1))?,
            _dialer: dialer,
            _running: running,
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
            .sync
            .publish(|peer| {
                peer.publication_mut().publish_incremental(
                    APP,
                    RECORD,
                    change,
                    &self.sk,
                    SystemTime::now(),
                )
            })
            .map_err(|error| anyhow!("publish an edit: {error}"))?;
        self.fleet.sent(&message, self.recipients);
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
            &AppPresence {
                peer: self.handle,
                app_id: APP,
                cluster_id: &self.workspace,
                presence: PeerPresence {
                    key: self.caret_key.clone(),
                    payload,
                },
                colour: Some(COLOUR),
            },
            timed,
        )
    }

    /// One pointer move along a fixed path.
    fn point(&self, step: u32) -> anyhow::Result<()> {
        self.presence(
            &PointerMove {
                peer: self.handle,
                user: &self.user,
                key: &self.pointer_key,
                point: (
                    f64::from(step).mul_add(3.0, 200.0),
                    f64::from(step.checked_rem(20).unwrap_or(0)).mul_add(2.0, 300.0),
                ),
                ts: tilo_clock::unix_now_ms(),
                colour: Some(COLOUR),
            },
            Some(Timed::Pointer),
        )
    }

    fn presence(
        &self,
        message: &impl serde::Serialize,
        timed: Option<Timed>,
    ) -> anyhow::Result<()> {
        let body = postcard::to_allocvec(message)?;
        if let Some(timed) = timed {
            self.fleet.presence_sent(&body, timed, self.recipients);
        }
        self.sync
            .publish(|_| {
                Ok(NeighbourMsg::LivePresence {
                    peer: self.handle,
                    frame: body.clone(),
                })
            })
            .map_err(|error| anyhow!("publish presence: {error}"))?;
        self.fleet.presence.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Run every member's minute from `start` until `end`; each task ends
/// a grace period after `end`, when the last edits had time to arrive.
pub fn play(
    members: Vec<Member>,
    start: Instant,
    end: Instant,
) -> Vec<JoinHandle<anyhow::Result<()>>> {
    members
        .into_iter()
        .map(|member| tokio::spawn(act(member, start, end)))
        .collect()
}

async fn act(mut member: Member, start: Instant, end: Instant) -> anyhow::Result<()> {
    let offset = u64::try_from(member.index)?
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
            member.type_one()?;
        }
        for step in 0..POINTER_MOVES_PER_MINUTE {
            let due = later(later(minute, POINTER_START, 1)?, POINTER_GAP, step)?;
            if due >= end {
                break 'run;
            }
            tokio::time::sleep_until(due.into()).await;
            member.point(step)?;
        }
        minute = minute.checked_add(MINUTE).context("clock")?;
    }
    tokio::time::sleep_until(end.checked_add(GRACE).context("clock")?.into()).await;
    Ok(())
}

fn later(base: Instant, gap: Duration, n: u32) -> anyhow::Result<Instant> {
    base.checked_add(gap.checked_mul(n).context("duration")?)
        .context("clock")
}
