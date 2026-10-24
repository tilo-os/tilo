use super::{
    Arc, AtomicBool, AtomicU64, CallTarget, Client, Command, Duration, Instant, Mutex,
    NativeWorkspaceOwner, OPEN_DEADLINE, Operation, Ordering, Reply, Ticket, Weak, authority,
    devices, observer, video,
};
use crate::{
    network::NetCmd,
    portal::{CaptureBroker, capture_broker::BorrowedCaptureHandle},
    shell::native_media::{MediaFactory, MediaSession, Prepare},
};
use media::voice::{
    self,
    gst::{CaptureSource, CaptureStop, ObservedOutput, VoiceAudio},
};
use runtime_collab::native_media::{
    Inventory, Issue, Picture, PictureSource, Shown, State as CallState,
};
use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::AsFd;
use tilo_moq::client::{self, Status};
use tilo_relay_protocol::voice::HEARTBEAT;
use tokio::sync::mpsc;

pub(crate) struct Factory {
    commands: mpsc::Sender<NetCmd>,
    owner: NativeWorkspaceOwner,
    devices: Arc<devices::Devices>,
    runtime: tokio::runtime::Handle,
    /// Woken by the network actor whenever a room's call list changes.
    rosters_changed: Arc<tokio::sync::Notify>,
    /// Where a call's camera pictures come from.
    camera: video::CameraChoice,
}
impl Factory {
    pub(crate) fn new(
        commands: mpsc::Sender<NetCmd>,
        owner: NativeWorkspaceOwner,
        broker: Arc<std::sync::Mutex<CaptureBroker>>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            commands,
            owner,
            devices: devices::Devices::start(broker, &runtime),
            runtime,
            rosters_changed: Arc::new(tokio::sync::Notify::new()),
            camera: video::CameraChoice::from_env(),
        }
    }
}
impl MediaFactory for Factory {
    fn rosters(&self, target: CallTarget) -> crate::shell::native_media::Rosters {
        let commands = self.commands.clone();
        let owner = self.owner.clone();
        let signal = self.rosters_changed.clone();
        Box::pin(async move {
            match command(
                &commands,
                Operation::Rosters {
                    owner,
                    target,
                    signal,
                },
            )
            .await
            {
                Ok(Reply::Rosters(rosters)) => rosters,
                _ => Vec::new(),
            }
        })
    }
    fn rosters_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.rosters_changed.clone())
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "`runtime` is moved into the returned session after the maintenance task and \
                  its abort handle are wired up; there is no earlier point to drop it"
    )]
    fn prepare(&self, target: CallTarget) -> Prepare {
        let commands = self.commands.clone();
        let owner = self.owner.clone();
        let devices = self.devices.clone();
        let runtime = self.runtime.clone();
        let camera = self.camera.clone();
        let channel = super::call_seal::CallChannel::of(&target);
        Box::pin(async move {
            let mut preparing = AbortTask(runtime.spawn(async move {
                // From here until the call ends the observations it reads
                // stay fresh; waiting for the grant can outlast them.
                let fresh = devices.watch();
                let observed = devices.prepare().await.map_err(|_| Issue::Unavailable)?;
                let output = devices.output().ok_or(Issue::Unavailable)?;
                if !observed.contains(output.cookie, output.node, output.serial, false) {
                    return Err(Issue::SourceLost);
                }
                let audio = VoiceAudio::new(ObservedOutput {
                    node: output.node,
                    serial: output.serial,
                    remote_fd: observed.socket.as_fd(),
                })
                .map_err(|_| Issue::Unavailable)?;
                // VoiceAudio retains its own descriptor for this exact socket.
                // The observer has already relinquished every protocol reader.
                drop(observed);
                let opened = command(&commands, Operation::Open { owner, target })
                    .await
                    .map_err(|_| {
                        tracing::debug!("native voice open command refused");
                        Issue::Denied
                    })?;
                let Reply::Opened(ticket) = opened else {
                    return Err(Issue::Unavailable);
                };
                if ticket.granted(OPEN_DEADLINE).await.is_err() {
                    tracing::debug!("native voice first grant unavailable");
                    ticket.stop();
                    return Err(Issue::Denied);
                }
                let runtime = Arc::new(Running {
                    ticket: ticket.clone(),
                    client: Mutex::new(None),
                    state: Mutex::new(Ok(CallState::Joining)),
                    cancelled: AtomicBool::new(false),
                    task: Mutex::new(None),
                    commands: commands.clone(),
                    executor: tokio::runtime::Handle::current(),
                });
                let controller = runtime.clone();
                let task = tokio::spawn(async move {
                    let result = maintain(&commands, &controller).await;
                    if !controller.cancelled.load(Ordering::Acquire) {
                        if result.is_err() {
                            let category = match result.as_ref().err().map(String::as_str) {
                                Some("native carrier retry budget exhausted") => {
                                    "carrier_retry_exhausted"
                                }
                                Some("native actor deadline") => "actor_deadline",
                                Some("the participation ended") => "participation_ended",
                                _ => "unavailable",
                            };
                            tracing::warn!(category, "native call carrier maintenance ended");
                        }
                        controller.ticket.stop();
                        *controller.state.lock() = result
                            .map(|()| CallState::Ended)
                            .map_err(|_| Issue::TransportFailed);
                    }
                });
                *runtime.task.lock() = Some(task.abort_handle());
                Ok(Box::new(Session {
                    runtime,
                    devices,
                    _fresh: fresh,
                    output,
                    audio,
                    capture: None,
                    decoded_generation: None,
                    first_packet_diagnosed: false,
                    next_audio_diagnostic: Instant::now(),
                    publishers: Vec::new(),
                    publications_revision: None,
                    heard: Heard::default(),
                    channel,
                    keys: None,
                    video: Video {
                        choice: camera,
                        ..Video::default()
                    },
                    volumes: BTreeMap::new(),
                    volumes_applied: None,
                    said_muted: true,
                    told: Arc::default(),
                }) as Box<dyn MediaSession>)
            }));
            (&mut preparing.0).await.map_err(|_| Issue::Unavailable)?
        })
    }
}
struct AbortTask<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(super) struct Running {
    pub(super) ticket: Arc<Ticket>,
    pub(super) client: Mutex<Option<Arc<Client>>>,
    pub(super) state: Mutex<Result<CallState, Issue>>,
    pub(super) cancelled: AtomicBool,
    pub(super) task: Mutex<Option<tokio::task::AbortHandle>>,
    pub(super) commands: mpsc::Sender<NetCmd>,
    pub(super) executor: tokio::runtime::Handle,
}
impl Running {
    fn retire(&self) {
        tracing::debug!("native carrier retirement started");
        self.ticket.clear_carrier();
        tracing::debug!("native carrier participation paused");
        let client = self.client.lock().take();
        if let Some(client) = client {
            client.close();
        }
        *self.state.lock() = Ok(CallState::Reconnecting);
        tracing::debug!("native carrier retirement completed");
    }
}
pub(super) async fn command(
    commands: &mpsc::Sender<NetCmd>,
    operation: Operation,
) -> Result<Reply, String> {
    let (reply, receive) = tokio::sync::oneshot::channel();
    tokio::time::timeout(Duration::from_secs(3), async {
        commands
            .send(NetCmd::NativeMedia(Command { operation, reply }))
            .await
            .map_err(|_| "native actor stopped".to_string())?;
        receive
            .await
            .map_err(|_| "native actor reply lost".to_string())?
    })
    .await
    .map_err(|_| "native actor deadline".to_string())?
}
/// Start a carrier for the current grant; `false` when it admits no route.
fn install_carrier(running: &Arc<Running>) -> Result<bool, String> {
    let ticket = &running.ticket;
    let Some(connection) = ticket.next_carrier()? else {
        return Ok(false);
    };
    if running.cancelled.load(Ordering::Acquire) {
        return Err("native session closed".into());
    }
    let client =
        Arc::new(Client::start(connection, ticket.clone()).map_err(|error| error.to_string())?);
    ticket.set_client(&client)?;
    *running.client.lock() = Some(client);
    Ok(true)
}
/// `Instant::now()` plus `delay`, saturating at now for an unrepresentable deadline.
fn later(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}
pub(super) async fn maintain(
    commands: &mpsc::Sender<NetCmd>,
    running: &Arc<Running>,
) -> Result<(), String> {
    let ticket = &running.ticket;
    let mut heartbeat = later(HEARTBEAT);
    let mut failures = 0_u32;
    let mut live_since = None;
    let mut retry = Instant::now();
    loop {
        let changed = ticket.changed().notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if running.cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        ticket.live()?;
        if !ticket.authorized() {
            return Err("native participation lapsed".into());
        }
        let now = Instant::now();
        // A new grant with another scope (roster, directions) is followed by
        // a new carrier; that is no failure.
        if ticket.stale() {
            live_since = None;
            running.retire();
            retry = now;
        }
        let status = running.client.lock().as_ref().map(|client| client.status());
        match status {
            Some(Status::Live) => {
                *running.state.lock() = Ok(CallState::Live);
                if now.duration_since(*live_since.get_or_insert(now)) >= Duration::from_secs(5) {
                    failures = 0;
                }
            }
            Some(Status::Connecting) => {
                live_since = None;
            }
            Some(Status::Failed | Status::Denied | Status::Closed) => {
                live_since = None;
                running.retire();
                failures = failures.saturating_add(1);
                if failures >= 3 {
                    return Err("native carrier retry budget exhausted".into());
                }
                retry = later(Duration::from_millis(200));
            }
            None if now >= retry && install_carrier(running).is_err() => {
                running.retire();
                failures = failures.saturating_add(1);
                if failures >= 3 {
                    return Err("native carrier retry budget exhausted".into());
                }
                retry = later(Duration::from_millis(200 << failures));
            }
            None => {}
        }
        if now >= heartbeat {
            command(
                commands,
                Operation::Heartbeat {
                    ticket: ticket.clone(),
                },
            )
            .await?;
            heartbeat = later(HEARTBEAT);
        }
        tokio::select! {
            () = changed => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
}

struct StopSlot {
    current: AtomicBool,
    expired: AtomicBool,
    startup: AtomicU64,
    ready: AtomicBool,
    generation: Mutex<Option<(CaptureStop, Weak<Client>)>>,
}
impl StopSlot {
    fn new() -> Self {
        Self {
            current: AtomicBool::new(true),
            expired: AtomicBool::new(false),
            startup: AtomicU64::new(0),
            ready: AtomicBool::new(false),
            generation: Mutex::new(None),
        }
    }
    fn begin(&self) -> Result<u64, Issue> {
        let _generation = self.generation.lock();
        if !self.current.load(Ordering::SeqCst) {
            return Err(self.stopped_issue());
        }
        self.advance()
    }
    // Caller holds generation while changing startup or checking its deadline.
    fn advance(&self) -> Result<u64, Issue> {
        self.ready.store(false, Ordering::SeqCst);
        self.startup
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Issue::CaptureFailed)
            .and_then(|value| value.checked_add(1).ok_or(Issue::CaptureFailed))
    }
    // A nonterminal owner fence retires this startup, but preserves the
    // explicit unmute intent. A terminal stop never becomes resumable.
    fn check_startup(&self, startup: u64) -> Result<bool, Issue> {
        if !self.current.load(Ordering::SeqCst) {
            return Err(self.stopped_issue());
        }
        Ok(self.startup.load(Ordering::SeqCst) == startup)
    }
    fn stopped_issue(&self) -> Issue {
        if self.expired.load(Ordering::SeqCst) {
            Issue::CaptureFailed
        } else {
            Issue::PermissionDenied
        }
    }
    fn pause(&self) -> u64 {
        tracing::debug!("native capture slot pause started");
        let mut generation = self.generation.lock();
        // A timer or prepared FD from the previous start cannot affect a later
        // resume of the same explicit unmute intent.
        if self.advance().is_err() {
            self.current.store(false, Ordering::SeqCst);
        }
        if let Some((capture, client)) = generation.take() {
            if let Some(client) = client.upgrade() {
                client.stop_publication();
            }
            capture.stop();
        }
        let idle = self.startup.load(Ordering::SeqCst);
        drop(generation);
        tracing::debug!("native capture slot pause completed");
        idle
    }
    fn stop(&self) {
        self.current.store(false, Ordering::SeqCst);
        self.pause();
    }
    fn expire(&self, startup: u64) {
        let mut generation = self.generation.lock();
        if self.startup.load(Ordering::SeqCst) == startup && !self.ready.load(Ordering::SeqCst) {
            tracing::warn!(
                installed = generation.is_some(),
                "native microphone capture startup deadline expired"
            );
            self.expired.store(true, Ordering::SeqCst);
            self.current.store(false, Ordering::SeqCst);
            let _ = self.advance();
            if let Some((capture, client)) = generation.take() {
                if let Some(client) = client.upgrade() {
                    client.stop_publication();
                }
                capture.stop();
            }
        }
    }
}
struct PendingCapture {
    receive: tokio::sync::oneshot::Receiver<Result<observer::Observed, String>>,
    task: tokio::task::AbortHandle,
}
impl Drop for PendingCapture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
struct CaptureIntent {
    source_id: String,
    source: Arc<devices::Observation>,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    slot: Arc<StopSlot>,
    startup: u64,
    pending: Option<PendingCapture>,
    prepared: Option<observer::Observed>,
    watchdog: Option<tokio::task::AbortHandle>,
}
impl CaptureIntent {
    fn cancel_pending(&mut self) {
        self.pending = None;
        self.prepared = None;
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.abort();
        }
    }
}
impl Drop for CaptureIntent {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}
fn installation_fence(
    intent: &CaptureIntent,
    ticket: Weak<Ticket>,
) -> Arc<dyn Fn() -> bool + Send + Sync> {
    let startup = intent.startup;
    let slot = Arc::downgrade(&intent.slot);
    let source = intent.source.clone();
    let allowed = intent.allowed.clone();
    Arc::new(move || {
        let (Some(slot), Some(ticket)) = (slot.upgrade(), ticket.upgrade()) else {
            return false;
        };
        slot.current.load(Ordering::SeqCst)
            && slot.startup.load(Ordering::SeqCst) == startup
            && allowed()
            && source.current()
            && ticket.live().is_ok()
            && !ticket.leaving()
            && ticket.authorized()
    })
}

/// Who the call hears talking, as of the last tick.
#[derive(Default, PartialEq, Eq)]
struct Heard {
    /// The other participants' namespaces, sorted.
    others: Vec<String>,
    /// This device's microphone.
    own: bool,
    /// Advances whenever the two above change.
    revision: u64,
}
impl Heard {
    fn update(&mut self, mut others: Vec<String>, own: bool) {
        others.sort();
        if others != self.others || own != self.own {
            self.others = others;
            self.own = own;
            self.revision = self.revision.wrapping_add(1);
        }
    }
}

struct Session {
    runtime: Arc<Running>,
    devices: Arc<devices::Devices>,
    /// Keeps `devices` refreshing for as long as the call lasts.
    _fresh: devices::Watch,
    output: Arc<devices::Observation>,
    audio: VoiceAudio,
    capture: Option<CaptureIntent>,
    decoded_generation: Option<u64>,
    first_packet_diagnosed: bool,
    next_audio_diagnostic: Instant,
    publishers: Vec<voice::PublicationId>,
    /// The carrier's reading revision the decoders were last matched to.
    publications_revision: Option<u64>,
    heard: Heard,
    /// Whose keys seal the call's audio and pictures.
    channel: Option<super::call_seal::CallChannel>,
    /// Those keys, as of this tick; `None` until this device holds them.
    keys: Option<channel_crypto::ChannelContentKeys>,
    video: Video,
    /// How loud each participant plays here, by `authority::call_id`.
    volumes: BTreeMap<String, u16>,
    /// The grant revision `volumes` was last worked out for, as publications.
    volumes_applied: Option<u64>,
    /// Whether the governor was last told this device is muted. A call
    /// starts muted, as its participation says from the start.
    said_muted: bool,
    told: Arc<Telling>,
}

/// What the governor is to be told this device shows. Tellings run one at
/// a time, in order, each sending what is newest when its turn comes, so
/// the governor always ends on the newest.
#[derive(Default)]
struct Telling {
    newest: Mutex<tilo_relay_protocol::voice::Showing>,
    turn: tokio::sync::Mutex<()>,
}

/// The call's pictures: what this device shows, what the app shows of the
/// others, and what arrived for it.
#[derive(Default)]
struct Video {
    choice: video::CameraChoice,
    camera: Option<video::Publisher>,
    screen: Option<video::Publisher>,
    /// What the app shows, by person.
    shown: Vec<Shown>,
    /// The carrier and grant revision `shown` was last applied to.
    applied: Option<(usize, u64)>,
    /// The publications asked for pictures at that time.
    showing: BTreeSet<String>,
    /// This device's person, as the grant names them.
    own: Option<String>,
    /// The publication this device sends, as the grant names it.
    publication: Option<String>,
    /// The grant revision `publication` was read at.
    publication_revision: Option<u64>,
    /// What the encoders seal with; `None` until this device holds the
    /// call's keys.
    sealing: Arc<Mutex<Option<super::call_seal::Sealing>>>,
    inbox: video::Inbox,
}
impl Session {
    // False requests guarded recovery from a nonterminal owner fence.
    fn install_prepared_capture(&mut self, client: &Arc<Client>) -> Result<bool, Issue> {
        let Some(intent) = &mut self.capture else {
            return Ok(true);
        };
        if !intent.slot.check_startup(intent.startup)? {
            intent.cancel_pending();
            return Ok(false);
        }
        if let Some(pending) = &mut intent.pending {
            match pending.receive.try_recv() {
                Ok(Ok(observed)) => {
                    intent.prepared = Some(observed);
                    intent.pending = None;
                }
                Ok(Err(_)) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    return Err(Issue::SourceLost);
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return Ok(true),
            }
        }
        let Some(observed) = &intent.prepared else {
            return Ok(true);
        };
        if !observed.contains(
            intent.source.cookie,
            intent.source.node,
            intent.source.serial,
            true,
        ) {
            return Err(Issue::SourceLost);
        }
        let startup = intent.startup;
        let slot = intent.slot.clone();
        let current = installation_fence(intent, Arc::downgrade(&self.runtime.ticket));
        // Fresh owner reads precede the independent slot lock. Owner fences
        // may hold their publication gate while synchronously stopping a slot.
        if client.enable_publication().is_err() {
            if !slot.check_startup(startup)? {
                return Ok(false);
            }
            tracing::warn!("native microphone publication authority refused");
            return Err(Issue::PermissionDenied);
        }
        let mut installing = slot.generation.lock();
        if !current() {
            client.stop_publication();
            if !slot.check_startup(startup)? {
                return Ok(false);
            }
            tracing::warn!("native microphone capture installation fence closed");
            return Err(Issue::PermissionDenied);
        }
        let (node, serial) = observed.call_capture_node(intent.source.node, intent.source.serial);
        match self.audio.start_capture(
            CaptureSource::PipeWire {
                node,
                serial,
                remote_fd: observed.socket.as_fd(),
            },
            current,
        ) {
            Ok(()) => {}
            Err(voice::AudioError::Capacity) => {
                // Capacity is checked before any worker takes this socket.
                // Keep it until the original two-second startup deadline.
                client.stop_publication();
                return Ok(true);
            }
            Err(_) => {
                client.stop_publication();
                return Err(Issue::CaptureFailed);
            }
        }
        let Some(stop) = self.audio.capture_stop_handle() else {
            self.audio.stop_capture();
            client.stop_publication();
            return Err(Issue::CaptureFailed);
        };
        *installing = Some((stop, Arc::downgrade(client)));
        drop(installing);
        intent.prepared = None;
        Ok(true)
    }
    fn retire_decoders(&mut self) {
        for publisher in self.publishers.drain(..) {
            self.audio.remove_publisher(&publisher);
        }
        self.decoded_generation = None;
        self.publications_revision = None;
        self.heard.update(Vec::new(), false);
        // A new carrier is asked for the shown pictures afresh, and every
        // stream starts over at a keyframe.
        self.video.applied = None;
        self.video.showing.clear();
        self.video.inbox.clear();
    }
    /// Play each participant at the volume set for them, by the
    /// publications of the current grant: someone back on a new
    /// publication keeps theirs.
    fn volume_tick(&mut self) {
        let revision = self.runtime.ticket.revision();
        if self.volumes_applied == Some(revision) {
            return;
        }
        let volumes = &self.volumes;
        let by_publication = self
            .runtime
            .ticket
            .inspect_current(|_, grant| {
                Ok(grant
                    .participants
                    .iter()
                    .filter_map(|participant| {
                        let percent = volumes.get(&authority::call_id(participant)?)?;
                        Some((participant.namespace.clone(), *percent))
                    })
                    .collect())
            })
            .unwrap_or_default();
        self.audio.set_volumes(by_publication);
        self.volumes_applied = Some(revision);
    }
    /// Ask the carrier for the pictures the app shows, by the publications
    /// of the people it shows, whenever the carrier or its grant changed;
    /// then move what arrived into the inbox, named by person.
    fn video_tick(&mut self, client: &Arc<Client>) {
        let carrier = Arc::as_ptr(client) as usize;
        let ticket = &self.runtime.ticket;
        let revision = ticket.revision();
        if self.video.applied != Some((carrier, revision)) {
            let shown = &self.video.shown;
            let (wanted, own) = ticket
                .inspect_current(|_, grant| {
                    let mut wanted: std::collections::BTreeMap<String, Vec<_>> =
                        std::collections::BTreeMap::new();
                    let mut own = None;
                    for participant in &grant.participants {
                        if grant.publication.as_ref() == Some(&participant.namespace) {
                            own = Some(hex::encode(participant.user_public_key));
                            continue;
                        }
                        // A bridge carries voices only, under its own person.
                        if participant.bridged.is_some() {
                            continue;
                        }
                        let person = hex::encode(participant.user_public_key);
                        let tracks: Vec<_> = shown
                            .iter()
                            .filter(|shown| shown.person == person)
                            .map(|shown| {
                                (video::source_of(shown.source), video::layer_of(shown.layer))
                            })
                            .collect();
                        if !tracks.is_empty() {
                            wanted.insert(participant.namespace.clone(), tracks);
                        }
                    }
                    Ok((wanted, own))
                })
                .unwrap_or_default();
            // This person's own camera tile is fed on this device.
            let own_layer = own.as_ref().and_then(|own| {
                shown
                    .iter()
                    .find(|shown| &shown.person == own && shown.source == PictureSource::Camera)
                    .map(|shown| video::layer_of(shown.layer))
            });
            if let Some(camera) = &self.video.camera {
                camera.preview().set(own_layer);
            }
            self.video.own = own;
            for namespace in &self.video.showing {
                if !wanted.contains_key(namespace) {
                    client.show(namespace, &[]);
                }
            }
            for (namespace, tracks) in &wanted {
                client.show(namespace, tracks);
            }
            self.video.showing = wanted.into_keys().collect();
            self.video.applied = Some((carrier, revision));
        }
        self.take_own_camera();
        for (source, publisher) in [
            (tilo_moq::video::Source::Camera, &self.video.camera),
            (tilo_moq::video::Source::Screen, &self.video.screen),
        ] {
            if let Some(publisher) = publisher
                && client.wants_picture_keyframe(source)
            {
                publisher.ask_keyframe();
            }
        }
        self.receive_pictures(client);
    }
    /// The pictures of others the app shows, opened with the call's keys,
    /// into its inbox; and the keys this device's encoders seal with.
    fn receive_pictures(&mut self, client: &Client) {
        let keys = self.keys.clone();
        let ticket = &self.runtime.ticket;
        for _ in 0..video::RECEIVE_PER_TICK {
            let Some(received) = client.try_recv_picture() else {
                break;
            };
            let (Some(keys), Some(channel)) = (&keys, self.channel) else {
                continue;
            };
            let namespace = &received.publisher.namespace;
            let Some(person) = ticket
                .inspect_current(|_, grant| {
                    Ok(grant
                        .participants
                        .iter()
                        .find(|participant| {
                            &participant.namespace == namespace && participant.bridged.is_none()
                        })
                        .map(|participant| hex::encode(participant.user_public_key)))
                })
                .ok()
                .flatten()
            else {
                continue;
            };
            let Some(opened) = super::call_seal::open(
                keys,
                channel.channel(),
                namespace,
                received.source,
                received.object,
            ) else {
                continue;
            };
            let Ok(object) = opened.encode() else {
                continue;
            };
            let keyframe = opened.keyframe;
            self.video.inbox.admit(
                Picture {
                    person,
                    stream: format!(
                        "{namespace}/{}/{}",
                        received.publisher.carrier_generation, received.publisher.incarnation
                    ),
                    source: video::picture_source(received.source),
                    object,
                },
                keyframe,
            );
        }
    }
    /// This person's own camera pictures, into the inbox their tile shows.
    fn take_own_camera(&mut self) {
        if let (Some(camera), Some(own)) = (&self.video.camera, &self.video.own) {
            const OWN: &str = "own/camera";
            let (objects, gap) = camera.preview().take();
            if gap {
                self.video.inbox.behind(OWN, PictureSource::Camera);
            }
            for object in objects {
                if let Ok(bytes) = object.encode() {
                    self.video.inbox.admit(
                        Picture {
                            person: own.clone(),
                            stream: OWN.into(),
                            source: PictureSource::Camera,
                            object: bytes,
                        },
                        object.keyframe,
                    );
                }
            }
        }
    }
    /// What this device shows now, as the governor is told: muted while
    /// it holds no microphone capture (a reconnect only pauses it).
    const fn showing(&self) -> tilo_relay_protocol::voice::Showing {
        tilo_relay_protocol::voice::Showing {
            camera: self.video.camera.is_some(),
            screen: self.video.screen.is_some(),
            muted: self.capture.is_none(),
            deafened: self.audio.deafened(),
        }
    }
    /// Tell the governor when this device was muted or unmuted.
    fn say_muted(&mut self) {
        let muted = self.capture.is_none();
        if muted != self.said_muted {
            self.said_muted = muted;
            self.say_showing();
        }
    }
    /// Tell the governor what this device shows, off the call's tick; not
    /// once the call is closed.
    fn say_showing(&self) {
        if self.runtime.cancelled.load(Ordering::Acquire) {
            return;
        }
        let ticket = self.runtime.ticket.clone();
        let commands = self.runtime.commands.clone();
        *self.told.newest.lock() = self.showing();
        let told = self.told.clone();
        self.runtime.executor.spawn(async move {
            let _turn = told.turn.lock().await;
            let showing = *told.newest.lock();
            if let Err(error) = command(&commands, Operation::Show { ticket, showing }).await {
                tracing::debug!(%error, "the call was not told what this device shows");
            }
        });
    }
    /// The call's keys, and what this device seals with: its publication as
    /// the current grant names it.
    fn refresh_keys(&mut self) {
        let revision = self.runtime.ticket.revision();
        if self.video.publication_revision != Some(revision) {
            self.video.publication = self
                .runtime
                .ticket
                .inspect_current(|_, grant| Ok(grant.publication.clone()))
                .ok()
                .flatten();
            self.video.publication_revision = Some(revision);
        }
        self.keys = self.channel.and_then(super::call_seal::CallChannel::keys);
        *self.video.sealing.lock() = self
            .keys
            .clone()
            .zip(self.video.publication.clone())
            .map(|(keys, publication)| super::call_seal::Sealing { keys, publication });
    }
    /// The carrier the call sends on now, for an encoder thread.
    fn current_client(&self) -> video::CurrentClient {
        let running = Arc::downgrade(&self.runtime);
        let sealing = self.video.sealing.clone();
        Arc::new(move || {
            let client = running.upgrade()?.client.lock().clone()?;
            Some((client, sealing.lock().clone()))
        })
    }
    /// Drop the playout and decoder of every publication the carrier no
    /// longer reads: someone left, or came back as a new incarnation.
    fn retire_departed(&mut self, client: &Client) {
        let revision = client.publications_revision();
        if self.publications_revision == Some(revision) {
            return;
        }
        self.publications_revision = Some(revision);
        let reading = client.publications();
        let audio = &mut self.audio;
        self.publishers.retain(|publisher| {
            let read = reading.iter().any(|current| {
                current.namespace == publisher.namespace
                    && current.carrier_generation == publisher.carrier_generation
                    && current.incarnation == publisher.incarnation
            });
            if !read {
                audio.remove_publisher(publisher);
            }
            read
        });
    }
}
impl MediaSession for Session {
    #[allow(
        clippy::too_many_lines,
        reason = "one call tick: carrier status, decoder generation, capture install, bounded \
                  send and receive; splitting it scatters the ordering the comments rely on"
    )]
    fn tick(&mut self, now: Instant) -> Result<CallState, Issue> {
        if !self.output.current() {
            tracing::debug!("native call output observation is no longer current");
            self.stop_capture();
            return Err(Issue::SourceLost);
        }
        self.runtime.ticket.live().map_err(|_| Issue::Denied)?;
        self.audio.playout(now).map_err(|_| Issue::Unavailable)?;
        if !self.audio.output_ready() {
            return Ok(CallState::Joining);
        }
        let status = *self.runtime.state.lock();
        let client = self.runtime.client.lock().clone();
        let Some(client) = client else {
            self.pause_capture();
            self.retire_decoders();
            return status;
        };
        match client.status() {
            Status::Live => {}
            Status::Connecting => {
                self.pause_capture();
                self.retire_decoders();
                return if status == Ok(CallState::Joining) {
                    status
                } else {
                    Ok(CallState::Reconnecting)
                };
            }
            _ => {
                self.pause_capture();
                self.retire_decoders();
                return Ok(CallState::Reconnecting);
            }
        }
        self.refresh_keys();
        let generation = self.runtime.ticket.carrier_generation();
        if self.decoded_generation != generation {
            for publisher in self.publishers.drain(..) {
                self.audio.remove_publisher(&publisher);
            }
            self.decoded_generation = generation;
        }
        if let Some(intent) = &self.capture {
            let capture_current = intent.slot.current.load(Ordering::SeqCst);
            let consent_current = (intent.allowed)();
            let source_current = intent.source.current();
            if !capture_current || !consent_current || !source_current {
                tracing::warn!(
                    capture_current,
                    consent_current,
                    source_current,
                    "native microphone capture fence closed"
                );
                let issue = if consent_current && source_current {
                    intent.slot.stopped_issue()
                } else {
                    Issue::PermissionDenied
                };
                self.stop_capture();
                return Err(issue);
            }
        }
        if !self.install_prepared_capture(&client)? {
            return Ok(CallState::Reconnecting);
        }
        if let Some(intent) = &self.capture {
            let installed = intent.slot.generation.lock().is_some();
            if installed {
                let frames = self
                    .audio
                    .poll_capture(now)
                    .map_err(|_| Issue::CaptureFailed)?;
                if now >= self.next_audio_diagnostic {
                    tracing::debug!(
                        frames = frames.len(),
                        "native voice capture poll progressed"
                    );
                    self.next_audio_diagnostic = later(Duration::from_secs(1));
                }
                let sealing = self.video.sealing.lock().clone();
                for frame in frames {
                    // Until this device holds the call's keys nothing leaves it.
                    let Some(sealing) = &sealing else {
                        continue;
                    };
                    let first_packet = !self.first_packet_diagnosed;
                    self.first_packet_diagnosed = true;
                    if first_packet {
                        tracing::debug!("native voice first packet entering carrier");
                    }
                    let header = (frame.timestamp_48k, frame.duration_samples);
                    let result = client.try_send_sealed(
                        client::CapturedOpus {
                            timestamp_48k: frame.timestamp_48k,
                            duration_samples: frame.duration_samples,
                            payload: frame.payload,
                            captured_at: frame.captured_at,
                        },
                        |sequence, payload| {
                            super::call_seal::seal_audio(sealing, sequence, header, &payload)
                        },
                    );
                    if first_packet {
                        tracing::debug!(
                            accepted = result.is_ok(),
                            expired = matches!(result, Err(client::ClientError::Expired)),
                            "native voice first packet carrier returned"
                        );
                    }
                    if !matches!(result, Ok(()) | Err(client::ClientError::Expired)) {
                        self.pause_capture();
                        return Ok(CallState::Reconnecting);
                    }
                }
                if self.audio.capture_ready() {
                    // The watchdog cannot race a new start: startup changes
                    // and readiness publication share the generation lock.
                    let _generation = intent.slot.generation.lock();
                    if intent.slot.startup.load(Ordering::SeqCst) == intent.startup {
                        intent.slot.ready.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        // Eight publishers times ten bounded frames: one tick never drains an
        // unbounded remote queue, and every original 200ms deadline is retained.
        for _ in 0..80 {
            let Some(frame) = client.try_recv() else {
                break;
            };
            let (Some(keys), Some(channel)) = (&self.keys, self.channel) else {
                continue;
            };
            let Some(payload) = super::call_seal::open_audio(
                keys,
                channel.channel(),
                &frame.publisher.namespace,
                &frame.object,
            ) else {
                continue;
            };
            // Carrier arrivals are asynchronous: this frame can arrive after
            // the tick began. Compare it with a fresh local clock observation,
            // retaining its original arrival and expiry without clamping either.
            let received_now = Instant::now();
            let publisher = voice::PublicationId {
                namespace: frame.publisher.namespace,
                carrier_generation: frame.publisher.carrier_generation,
                incarnation: frame.publisher.incarnation,
            };
            if !self.publishers.contains(&publisher) {
                // A returning publisher's new incarnation replaces its last.
                self.publishers
                    .retain(|known| known.namespace != publisher.namespace);
                self.publishers.push(publisher.clone());
            }
            self.audio
                .receive(
                    voice::ReceivedOpus {
                        publisher,
                        sequence: frame.object.sequence,
                        timestamp_48k: frame.object.timestamp,
                        duration_samples: frame.object.duration,
                        payload,
                        received_at: frame.received_at,
                        expires_at: frame.expires_at,
                    },
                    received_now,
                )
                .map_err(|error| {
                    tracing::debug!(?error, "native voice receive admission failed");
                    Issue::TransportFailed
                })?;
        }
        self.retire_departed(&client);
        self.video_tick(&client);
        self.volume_tick();
        let now = Instant::now();
        self.audio.playout(now).map_err(|_| Issue::Unavailable)?;
        let others = self
            .audio
            .heard(now)
            .into_iter()
            .map(|publisher| publisher.namespace)
            .collect();
        let own = self.audio.capture_heard(now);
        self.heard.update(others, own);
        Ok(CallState::Live)
    }
    fn speaking_revision(&self) -> u64 {
        self.heard.revision
    }
    fn speaking(&self) -> Vec<String> {
        if self.heard.others.is_empty() && !self.heard.own {
            return Vec::new();
        }
        authority::speakers(&self.runtime.ticket, &self.heard.others, self.heard.own)
    }
    fn inventory(&self) -> Inventory {
        if self.audio.output_ready()
            && self
                .runtime
                .client
                .lock()
                .as_ref()
                .is_some_and(|client| client.status() == Status::Live)
        {
            authority::inventory(&self.runtime.ticket)
        } else {
            Inventory::default()
        }
    }
    fn start_capture(
        &mut self,
        source_id: &str,
        handle: BorrowedCaptureHandle<'_>,
        allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), Issue> {
        let BorrowedCaptureHandle::PipeWireNode(node) = handle else {
            return Err(Issue::SourceLost);
        };
        let source = self
            .devices
            .source(source_id, node)
            .ok_or(Issue::SourceLost)?;
        let client = self
            .runtime
            .client
            .lock()
            .clone()
            .ok_or(Issue::TransportFailed)?;
        if client.status() != Status::Live || !allowed() {
            return Err(Issue::PermissionDenied);
        }
        let reuse = self.capture.as_ref().is_some_and(|intent| {
            intent.source_id == source_id
                && Arc::ptr_eq(&intent.allowed, &allowed)
                && Arc::ptr_eq(&intent.source, &source)
                && intent.slot.current.load(Ordering::SeqCst)
        });
        if !reuse {
            // Leaves `self.capture` empty, so the intent below is always
            // fresh; the person stays unmuted throughout a source change.
            if let Some(intent) = self.capture.take() {
                intent.slot.stop();
            }
            self.audio.stop_capture();
        }
        let intent = self.capture.get_or_insert_with(|| CaptureIntent {
            source_id: source_id.into(),
            source,
            allowed: allowed.clone(),
            slot: Arc::new(StopSlot::new()),
            startup: 0,
            pending: None,
            prepared: None,
            watchdog: None,
        });
        let starting = intent.pending.is_some()
            || intent.prepared.is_some()
            || intent.slot.generation.lock().is_some();
        self.say_muted();
        let Some(intent) = self.capture.as_mut().filter(|_| !starting) else {
            return Ok(());
        };
        intent.cancel_pending();
        let slot = intent.slot.clone();
        let startup = slot.begin()?;
        intent.startup = startup;
        let weak = Arc::downgrade(&slot);
        self.runtime
            .ticket
            .set_capture_stop(Arc::new(move |terminal| {
                if let Some(slot) = weak.upgrade() {
                    if terminal {
                        slot.stop();
                    } else {
                        slot.pause();
                    }
                }
            }));
        let devices = self.devices.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        let preparing = self.runtime.executor.spawn(async move {
            let _ = send.send(devices.prepare().await);
        });
        intent.pending = Some(PendingCapture {
            receive,
            task: preparing.abort_handle(),
        });
        let started = tokio::time::Instant::now();
        let deadline = started
            .checked_add(Duration::from_secs(2))
            .unwrap_or(started);
        let watchdog = self.runtime.executor.spawn(async move {
            tokio::time::sleep_until(deadline).await;
            slot.expire(startup);
        });
        intent.watchdog = Some(watchdog.abort_handle());
        Ok(())
    }
    fn capture_stop(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let slot = self.capture.as_ref()?.slot.clone();
        Some(Arc::new(move || slot.stop()))
    }
    fn capture_ready(&self) -> bool {
        self.audio.capture_ready()
            && self.capture.as_ref().is_some_and(|intent| {
                intent.slot.current.load(Ordering::SeqCst)
                    && intent.slot.ready.load(Ordering::SeqCst)
                    && intent.slot.startup.load(Ordering::SeqCst) == intent.startup
                    && intent.source.current()
                    && (intent.allowed)()
            })
    }
    fn pause_capture(&mut self) {
        if let Some(intent) = &mut self.capture {
            intent.cancel_pending();
            // Acknowledge the retired generation without starting capture.
            // Otherwise a live replacement carrier keeps returning Reconnecting
            // for this stale startup, while the service waits for Live before
            // restarting under the existing current consent/source lease.
            intent.startup = intent.slot.pause();
        }
        self.audio.stop_capture();
    }
    fn stop_capture(&mut self) {
        if let Some(intent) = self.capture.take() {
            intent.slot.stop();
        }
        self.audio.stop_capture();
        self.say_muted();
    }
    fn start_video(&mut self, source: PictureSource) -> Result<(), Issue> {
        let running = self.runtime.client.lock().clone();
        if running.is_none_or(|client| client.status() != Status::Live) {
            return Err(Issue::TransportFailed);
        }
        let slot = match source {
            PictureSource::Camera => &self.video.camera,
            PictureSource::Screen => &self.video.screen,
        };
        if slot.is_some() {
            return Ok(());
        }
        let current = self.current_client();
        let publisher = match source {
            PictureSource::Camera => video::Publisher::camera(&self.video.choice, current),
            PictureSource::Screen => video::Publisher::screen(current),
        }
        .map_err(|error| {
            tracing::warn!(?source, %error, "the call's picture did not start");
            match source {
                PictureSource::Camera => Issue::CameraLost,
                PictureSource::Screen => Issue::ScreenLost,
            }
        })?;
        match source {
            PictureSource::Camera => self.video.camera = Some(publisher),
            PictureSource::Screen => self.video.screen = Some(publisher),
        }
        // The own tile's picture is set up with the next tick.
        self.video.applied = None;
        self.say_showing();
        Ok(())
    }
    fn stop_video(&mut self, source: PictureSource) {
        let stopped = match source {
            PictureSource::Camera => self.video.camera.take(),
            PictureSource::Screen => self.video.screen.take(),
        };
        if stopped.is_some() {
            self.say_showing();
        }
    }
    fn video_lost(&mut self) -> Option<PictureSource> {
        if self
            .video
            .camera
            .as_ref()
            .is_some_and(video::Publisher::failed)
        {
            self.stop_video(PictureSource::Camera);
            return Some(PictureSource::Camera);
        }
        None
    }
    fn set_deafened(&mut self, deafened: bool) {
        if self.audio.deafened() != deafened {
            self.audio.set_deafened(deafened);
            self.say_showing();
        }
    }
    fn show(&mut self, shown: Vec<Shown>) {
        if self.video.shown != shown {
            self.video.shown = shown;
            self.video.applied = None;
        }
    }
    fn set_volume(&mut self, participant: String, percent: u16) {
        if percent == 100 {
            self.volumes.remove(&participant);
        } else {
            self.volumes.insert(participant, percent);
        }
        self.volumes_applied = None;
    }
    fn take_pictures(&mut self, after: u64, budget: usize) -> (Vec<Picture>, u64) {
        self.video.inbox.take(after, budget)
    }
    fn pictures_signal(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(self.video.inbox.signal())
    }
    fn close(&mut self) {
        // First, so nothing tells the governor what a leaving call shows.
        let first = !self.runtime.cancelled.swap(true, Ordering::SeqCst);
        self.stop_capture();
        self.video.camera = None;
        self.video.screen = None;
        self.runtime.ticket.stop();
        let task = self.runtime.task.lock().take();
        if let Some(task) = task {
            task.abort();
        }
        let client = self.runtime.client.lock().take();
        if let Some(client) = client {
            client.close();
        }
        self.audio.close();
        *self.runtime.state.lock() = Ok(CallState::Ended);
        if first {
            let ticket = self.runtime.ticket.clone();
            let commands = self.runtime.commands.clone();
            // Best-effort leave; the local stop above never waits for the
            // network, and the governor ends a silent participation anyway.
            self.runtime.executor.spawn(async move {
                let _ = command(&commands, Operation::End { ticket }).await;
            });
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_capture_pause_reconnects_but_terminal_stop_refuses_resume() {
        let slot = StopSlot::new();
        let first = slot.begin().unwrap();
        assert_eq!(slot.check_startup(first), Ok(true));
        slot.pause();
        assert_eq!(slot.check_startup(first), Ok(false));
        slot.expire(first);
        let resumed = slot.begin().unwrap();
        assert_eq!(slot.check_startup(resumed), Ok(true));
        slot.stop();
        assert_eq!(slot.check_startup(resumed), Err(Issue::PermissionDenied));
        assert_eq!(slot.begin(), Err(Issue::PermissionDenied));
    }

    #[test]
    fn acknowledged_idle_generation_is_not_ready_and_cannot_outlive_terminal_stop() {
        let slot = StopSlot::new();
        let old = slot.begin().unwrap();
        slot.ready.store(true, Ordering::SeqCst);
        let idle = slot.pause();
        assert_eq!(slot.check_startup(old), Ok(false));
        assert_eq!(slot.check_startup(idle), Ok(true));
        assert!(!slot.ready.load(Ordering::SeqCst));
        assert!(slot.generation.lock().is_none());
        // The old watchdog cannot revoke idle recovery or a fresh startup.
        slot.expire(old);
        let resumed = slot.begin().unwrap();
        assert_ne!(resumed, idle);
        assert_eq!(slot.check_startup(idle), Ok(false));
        assert_eq!(slot.check_startup(resumed), Ok(true));
        slot.stop();
        assert_eq!(slot.check_startup(resumed), Err(Issue::PermissionDenied));
        assert_eq!(slot.begin(), Err(Issue::PermissionDenied));
    }

    #[test]
    fn paused_startup_timeout_cannot_revoke_resumed_intent_but_current_timeout_does() {
        let slot = StopSlot::new();
        let old = slot.begin().unwrap();
        slot.pause();
        let resumed = slot.begin().unwrap();
        slot.expire(old);
        assert!(slot.current.load(Ordering::SeqCst));
        slot.expire(resumed);
        assert!(!slot.current.load(Ordering::SeqCst));
        assert_eq!(slot.check_startup(resumed), Err(Issue::CaptureFailed));
        assert_eq!(slot.begin(), Err(Issue::CaptureFailed));
    }

    #[test]
    fn observed_capture_readiness_survives_its_deadline_but_leave_is_terminal() {
        let slot = StopSlot::new();
        let startup = slot.begin().unwrap();
        slot.ready.store(true, Ordering::SeqCst);
        slot.expire(startup);
        assert!(slot.current.load(Ordering::SeqCst));
        slot.stop();
        assert!(!slot.current.load(Ordering::SeqCst));
        assert!(slot.begin().is_err());
    }
}
