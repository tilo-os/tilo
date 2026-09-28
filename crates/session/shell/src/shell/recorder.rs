//! The session recorder: a silent follower that keeps what it sees.
//!
//! The recorder is an ordinary agent principal, [`RECORDER_AGENT`], admitted
//! with the Observer admission to exactly the rooms the user chose to
//! record. It receives what a read-only member receives — the broker's
//! in-process fanout, on the same delivery predicate as a socket peer — and
//! it appears in the collaborator popover as an agent for as long as it
//! holds a room. That is the recording indicator; removing the agent in
//! Workspace Settings revokes it, and the next [`Recorder::pump`] lets go of
//! the room.
//!
//! Stopping produces a `session` record ([`session_trace`]) homed in the
//! personal room. Nothing is written or sent: the record waits in memory for
//! review ([`Recorder::awaiting_review`]).
//!
//! A recording may keep pixels too ([`PixelPurpose`]): the compositor then
//! hands the recorded surfaces' frames to [`Recorder::push_frame`], masked
//! on arrival, and the sealed session references the frame strip as a
//! blob with relation `frames`. The strip's bytes stay with the pending
//! session in memory, like the record itself.
//!
//! A capture-protected app ([`crate::shell::capture_protection`]) is
//! withheld at intake: its snapshots, ops, presence, keyframes, geometry and
//! frames are dropped as they arrive, and the session keeps only a marker
//! that it was present and protected ([`session_trace::ProtectedApp`]).
//!
//! With the recent buffer on (`feedback.recent_buffer`, off by default), the
//! recorder also follows the rooms the user administers as they visit them
//! and keeps the last two minutes of presence and op references in memory,
//! from which [`Recorder::capture_recent`] builds a session.

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use automerge::AutoCommit;
use session_trace::frames::{
    FrameLimits, FramePush, FrameStrip, FrameStripBuilder, PixelPurpose, PreparedFrame,
    SurfaceFrame,
};
use session_trace::{
    Environment, ImageDigest, ImageIdentity, Limits, OpReference, PresenceChange, PresenceUpdate,
    Pushed, RecentBuffer, RecentItem, Route, SessionSummary, SessionTrace, SurfaceGeometry,
    TraceBuilder,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordRef};

use crate::shell::collab_broker::{
    BrokerChannels, BrokerHandle, CrdtRoute, FanoutState, LocalParticipant, RoomSubscription,
};
use crate::shell::state::{
    CapOverride, CapRole, Principal, Subject, UserId, Verb, WorkspaceId, WorkspaceTree,
    WorkspaceTreeHandle,
};

/// The recorder's agent principal id: the capability subject it is admitted
/// as, and its roster id.
pub const RECORDER_AGENT: &str = "recorder";
/// What the roster calls it.
pub const RECORDER_LABEL: &str = "Recorder";
/// The admission the recorder grants itself, in the shape every agent
/// admission has (`agent-member:<agent>:<preset>`), so Workspace Settings
/// shows it as an Observer and Remove revokes it like any agent.
pub const RECORDER_ADMISSION: &str = "agent-member:recorder:observer";
/// Settings section and key of the recent-buffer consent.
pub const RECENT_BUFFER_SECTION: &str = "feedback";
pub const RECENT_BUFFER_KEY: &str = "recent_buffer";
/// How long a capture waits for end-of-window snapshots.
const CAPTURE_SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
/// An app that does not report semantic changes is asked this often
/// instead, and its answer kept only if the tree changed.
const KEYFRAME_FALLBACK_POLL: Duration = Duration::from_secs(2);
/// A keyframe waits at least this long after the previous one of the same
/// app, so a burst of changes is one keyframe of where it settled.
const KEYFRAME_MIN_GAP: Duration = Duration::from_millis(250);
/// Depth of the subtree a keyframe holds.
const KEYFRAME_DEPTH: u16 = 16;
/// Keyframes kept while the recorder holds rooms, across all of them.
const MAX_KEYFRAMES: usize = 1_024;
/// Sessions kept for review before the oldest is dropped.
const MAX_AWAITING_REVIEW: usize = 8;

/// Why a recorder request was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecorderError {
    #[error("already recording; stop the current recording first")]
    AlreadyRecording,
    #[error("nothing is being recorded")]
    NotRecording,
    #[error("choose at least one room to record")]
    NoRooms,
    #[error("room {0} is not a workspace on this machine")]
    UnknownRoom(String),
    #[error("{user} is not a member of room {room}, so the recorder cannot act for them there")]
    NotMember { user: String, room: String },
    #[error(
        "“{0}” does not allow its members to record. An administrator can turn on “Members may record” in Workspace Settings › Agents."
    )]
    RecordingNotAllowed(String),
    #[error("the recorder was removed from room {0}; admit it again from Workspace Settings")]
    Removed(String),
    #[error("the recent buffer is off; turn on feedback.recent_buffer first")]
    RecentBufferOff,
    #[error("nothing happened in the recent buffer's window")]
    NothingRecent,
    #[error("a capture is already being finished")]
    CaptureInProgress,
    #[error("the session could not be written: {0}")]
    Write(String),
}

/// One app surface as the shell measured it this frame: the active tab of
/// a window group, the same way in the native and the hosted compositor.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredSurface {
    pub tab: crate::shell::state::TabId,
    /// Logical size of its tile.
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub output: String,
}

/// One display the shell draws onto, in scene coordinates: a monitor in the
/// native compositor, the host window in the hosted one.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedOutput {
    pub rect: vello::kurbo::Rect,
    pub scale: f64,
    pub name: String,
}

/// Whether `app_id` is capture-protected, so the recorder keeps nothing of
/// it but a marker.
fn is_protected(app_id: &str) -> bool {
    !app_id.is_empty() && crate::shell::foreign::store().is_app_capture_protected(app_id)
}

/// The app a tab shows, by the id its surface and grants use.
fn tab_app_id(tile: &crate::shell::state::TileContent) -> Option<String> {
    use crate::shell::state::TileContent;
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Some(app_id.clone()),
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(kind.app_id())
        }
        TileContent::WorkspaceActivity => None,
    }
}

/// A finished session waiting for review. Held only in memory.
pub struct PendingSession {
    pub checkout: RecordCheckout,
    pub summary: SessionSummary,
    doc: AutoCommit,
    frames: Option<SealedFrames>,
    /// What replaying the session found against its frames, once checked.
    divergence: Option<session_trace::divergence::DivergenceReport>,
}

/// The frame strip of a pending session and the blob bytes its record
/// references.
struct SealedFrames {
    strip: FrameStrip,
    blob: Vec<u8>,
}

impl PendingSession {
    /// The frames the recording kept, masked, if it kept pixels.
    pub fn frames(&self) -> Option<&FrameStrip> {
        self.frames.as_ref().map(|frames| &frames.strip)
    }

    /// The bytes of the blob the record references with relation
    /// `frames`: what review stores or attaches.
    pub fn frame_blob(&self) -> Option<&[u8]> {
        self.frames.as_ref().map(|frames| frames.blob.as_slice())
    }

    /// What replaying the session found against its frames, when it has
    /// been checked.
    pub const fn divergence(&self) -> Option<&session_trace::divergence::DivergenceReport> {
        self.divergence.as_ref()
    }

    /// The record document: identity plus the `session` map.
    pub fn document(&self) -> &AutoCommit {
        &self.doc
    }

    /// The full trace, decoded from the record document.
    ///
    /// # Errors
    ///
    /// When the document no longer holds a readable session.
    pub fn trace(&self) -> Result<SessionTrace, session_trace::TraceError> {
        session_trace::read(&self.doc)
    }
}

/// What the sink thread writes into.
#[derive(Default)]
struct Capture {
    recording: Option<Recording>,
    recent: Option<RecentBuffer>,
    recent_rooms: BTreeSet<String>,
    finishing: Option<Finishing>,
    /// Latest geometry of each app surface in a held room.
    surfaces: Vec<(String, SurfaceGeometry)>,
    /// The scale each measured tab's surface renders at: what a captured
    /// frame's pixels are per logical pixel.
    surface_scales: Vec<(crate::shell::state::TabId, f64)>,
    /// Capture-protected apps seen in a held room, as `(room, app)`: all a
    /// session keeps of them.
    protected: BTreeSet<(String, String)>,
}

impl Capture {
    /// `app_id` is present in `room` and protected: remember it for a later
    /// capture of the recent buffer and mark the recording that holds it.
    fn note_protected(&mut self, room: &str, app_id: &str) {
        let recorded = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.rooms.contains(room) && recording.wants_app(app_id));
        if !recorded && !self.recent_rooms.contains(room) {
            return;
        }
        if recorded && let Some(recording) = self.recording.as_mut() {
            recording.builder.note_protected(app_id, room);
        }
        if let Some(finishing) = self.finishing.as_mut() {
            finishing.builder.note_protected(app_id, room);
        }
        self.protected
            .insert((room.to_string(), app_id.to_string()));
    }
}

struct Recording {
    rooms: BTreeSet<String>,
    /// Only these apps' routes, or every route when empty.
    apps: Vec<String>,
    builder: TraceBuilder,
    /// Present when the recording keeps pixels.
    frames: Option<FrameStripBuilder>,
}

impl Recording {
    fn wants(&self, route: &CrdtRoute) -> bool {
        self.rooms.contains(&route.cluster_id) && self.wants_app(&route.app_id)
    }

    fn wants_app(&self, app_id: &str) -> bool {
        self.apps.is_empty() || self.apps.iter().any(|app| app == app_id)
    }
}

/// Which surfaces the compositor should capture frames of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelRequest {
    pub rooms: Vec<String>,
    /// Only these apps, or every app when empty.
    pub apps: Vec<String>,
    pub purpose: PixelPurpose,
}

impl PixelRequest {
    pub fn wants(&self, room: &str, app_id: &str) -> bool {
        self.rooms.iter().any(|wanted| wanted == room)
            && (self.apps.is_empty() || self.apps.iter().any(|app| app == app_id))
    }
}

/// A retroactive capture waiting for its end-of-window snapshots.
struct Finishing {
    builder: TraceBuilder,
    awaiting: Vec<Route>,
    deadline: Instant,
}

/// State only the frame thread touches.
#[derive(Default)]
struct Control {
    /// Admissions this recorder was given, by room — the workspace and the
    /// capability id — so it revokes only its own.
    granted: BTreeMap<String, (WorkspaceId, String)>,
    /// Rooms the user removed the recorder from while the buffer followed
    /// them; not re-admitted until the buffer is switched on again.
    refused: BTreeSet<String>,
    /// Rooms the subscription covers, last time it was pointed.
    held: BTreeSet<String>,
    awaiting_review: Vec<PendingSession>,
    recent_enabled: bool,
    settings_stamp: Option<SystemTime>,
    /// Inspect keyframes in flight and what was last kept, when the broker
    /// serves inspection.
    keyframes: Option<Keyframes>,
}

/// Semantic-tree keyframes: each inspectable app in a held room is asked for
/// its tree when it reports that the tree changed (or, for an app that does
/// not report, on a slow poll), debounced, and the answer is kept only when
/// it differs from the last one.
struct Keyframes {
    broker: crate::shell::collab_broker::AppActionBroker,
    /// The broker's push of semantic-tree changes.
    changes: std::sync::mpsc::Receiver<crate::shell::collab_broker_capnp::SemanticChange>,
    /// Latest generation each connection reported.
    reported: BTreeMap<u64, u64>,
    /// Generation each connection had when it was last asked.
    inspected: BTreeMap<u64, u64>,
    /// Connections asked for a keyframe out of turn (a captured frame).
    triggered: BTreeSet<u64>,
    pending: Vec<PendingKeyframe>,
    asked: BTreeMap<Route, Instant>,
    last: BTreeMap<Route, ui::inspect::InspectReply>,
    kept: usize,
}

struct PendingKeyframe {
    route: Route,
    job: crate::shell::collab_broker_capnp::InspectionJob,
    asked: Instant,
}

impl Keyframes {
    fn new(broker: crate::shell::collab_broker::AppActionBroker) -> Self {
        Self {
            changes: broker.watch_semantic_changes(),
            broker,
            reported: BTreeMap::new(),
            inspected: BTreeMap::new(),
            triggered: BTreeSet::new(),
            pending: Vec::new(),
            asked: BTreeMap::new(),
            last: BTreeMap::new(),
            kept: 0,
        }
    }

    /// Answers that arrived, kept only where the tree changed.
    fn collect(&mut self, now: Instant) -> Vec<session_trace::InspectKeyframe> {
        let mut answered = Vec::new();
        self.pending
            .retain(|pending| match pending.job.try_answer() {
                Some(Ok(reply)) => {
                    answered.push((pending.route.clone(), reply));
                    false
                }
                Some(Err(error)) => {
                    tracing::debug!(%error, "recorder: a keyframe was not answered");
                    false
                }
                None => {
                    now.saturating_duration_since(pending.asked)
                        < crate::shell::collab_broker_capnp::INSPECTION_TIMEOUT
                }
            });
        let mut keep = Vec::new();
        for (route, reply) in answered {
            // Protection that began after the ask still withholds the tree.
            if is_protected(&route.app_id) {
                continue;
            }
            // A keyframe is a change of structure, not a heartbeat.
            if self.last.get(&route) == Some(&reply) || self.kept >= MAX_KEYFRAMES {
                continue;
            }
            self.last.insert(route.clone(), reply.clone());
            self.kept = self.kept.saturating_add(1);
            keep.push(session_trace::InspectKeyframe { route, reply });
        }
        keep
    }

    /// Take in every change report since the last frame.
    fn drain_changes(&mut self) {
        while let Ok(change) = self.changes.try_recv() {
            let seen = self.reported.entry(change.peer_id).or_insert(0);
            *seen = (*seen).max(change.generation);
        }
    }

    fn forget(&mut self) {
        self.inspected.clear();
        self.triggered.clear();
        self.asked.clear();
        self.last.clear();
        self.kept = 0;
    }

    /// Ask each inspectable app in `held` whose keyframe is due: its tree
    /// changed since it was last asked (or it was never asked), or a frame
    /// of it was captured, or — for an app that reports no changes — the
    /// fallback poll came round; never sooner than [`KEYFRAME_MIN_GAP`].
    ///
    /// Returns the protected apps found in `held`, as `(room, app)`, which
    /// were not asked.
    fn ask(&mut self, held: &[String], now: Instant, user: &UserId) -> Vec<(String, String)> {
        let mut protected = Vec::new();
        self.drain_changes();
        let caller = crate::shell::collab_broker_capnp::InspectCaller {
            person: user.clone(),
            audience: ui::a11y::Audience::LocalSession,
        };
        for app in self.broker.inspectable_apps() {
            if !held.contains(&app.room) {
                continue;
            }
            // A protected app's tree is never asked for.
            if is_protected(&app.app_id) {
                protected.push((app.room.clone(), app.app_id.clone()));
                continue;
            }
            if self.kept >= MAX_KEYFRAMES {
                continue;
            }
            let route = Route::new(app.app_id.clone(), app.room.clone(), app.record.clone());
            if self.pending.iter().any(|pending| pending.route == route) {
                continue;
            }
            let generation = self
                .reported
                .get(&app.peer_id)
                .copied()
                .unwrap_or(0)
                .max(app.semantic_generation);
            let since = self
                .asked
                .get(&route)
                .map_or(Duration::MAX, |asked| now.saturating_duration_since(*asked));
            if !keyframe_due(
                generation,
                self.inspected.get(&app.peer_id).copied(),
                since,
                self.triggered.contains(&app.peer_id),
            ) {
                continue;
            }
            self.asked.insert(route.clone(), now);
            self.inspected.insert(app.peer_id, generation);
            self.triggered.remove(&app.peer_id);
            match self.broker.inspect(
                app.peer_id,
                ui::inspect::InspectQuery::Subtree {
                    root: None,
                    depth: KEYFRAME_DEPTH,
                },
                &caller,
            ) {
                Ok(job) => self.pending.push(PendingKeyframe {
                    route,
                    job,
                    asked: now,
                }),
                Err(error) => {
                    tracing::debug!(%error, "recorder: could not ask for a keyframe");
                }
            }
        }
        protected
    }
}

/// Whether an app's keyframe is due: its reported `generation` moved past
/// the one it was last `inspected` at (or it was never asked), or a frame
/// of it was captured (`triggered`), or it reports nothing (generation 0)
/// and the fallback poll came round — and never within [`KEYFRAME_MIN_GAP`]
/// of the last ask.
fn keyframe_due(generation: u64, inspected: Option<u64>, since: Duration, triggered: bool) -> bool {
    let changed = inspected.is_none_or(|inspected| generation > inspected);
    let fallback = generation == 0 && since >= KEYFRAME_FALLBACK_POLL;
    since >= KEYFRAME_MIN_GAP && (changed || triggered || fallback)
}

/// The session recorder. One per session; shared by the frame loop and the
/// shell action catalogue.
pub struct Recorder {
    tree: WorkspaceTreeHandle,
    fanout: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
    /// Opens what the recorded rooms carry; it cannot write there.
    opener: crate::shell::participant_keys::ChannelOpener,
    /// Typed participant id for snapshot requests, so the broker never picks
    /// the recorder itself as a snapshot holder.
    peer_id: u64,
    capture: Arc<Mutex<Capture>>,
    subscription: Arc<RoomSubscription>,
    control: Mutex<Control>,
    limits: Limits,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Recorder {
    /// Attach to the session broker. Holds no room until one is recorded.
    pub fn attach(broker: &BrokerHandle, tree: WorkspaceTreeHandle) -> Arc<Self> {
        Self::attach_with_limits(broker, tree, Limits::default())
    }

    pub fn attach_with_limits(
        broker: &BrokerHandle,
        tree: WorkspaceTreeHandle,
        limits: Limits,
    ) -> Arc<Self> {
        warm_image_digest();
        Self::drop_stale_delegations(&tree);
        let fanout = broker.fanout();
        let capture: Arc<Mutex<Capture>> = Arc::new(Mutex::new(Capture::default()));
        let sink_capture = Arc::clone(&capture);
        // The recorder is an Observer: it opens what the rooms it records
        // carry, and holds nothing that could write there.
        let opener =
            crate::shell::participant_keys::ParticipantKeys::for_broker(broker, Arc::clone(&tree))
                .opener(crate::shell::participant_keys::Participant::Agent(
                    RECORDER_AGENT.into(),
                ));
        let sink_opener = opener.clone();
        let subscription = fanout.subscribe_rooms_as(
            LocalParticipant {
                agent: RECORDER_AGENT.into(),
                label: RECORDER_LABEL.into(),
            },
            Vec::new(),
            Box::new(move |event, route| {
                let readable = opened_event(&sink_opener, event, route);
                observe(
                    &mut lock(&sink_capture),
                    Instant::now(),
                    readable.as_ref().unwrap_or(event),
                    route,
                );
            }),
        );
        let peer_id = fanout.next_peer_id();
        Arc::new(Self {
            tree,
            fanout,
            channels: broker.channels(),
            opener,
            peer_id,
            capture,
            subscription,
            control: Mutex::new(Control {
                keyframes: broker.app_action_broker().map(Keyframes::new),
                ..Control::default()
            }),
            limits,
        })
    }

    /// Rooms the recorder currently holds, and so is listed in.
    pub fn rooms(&self) -> Vec<String> {
        self.subscription.rooms()
    }

    pub fn is_recording(&self) -> bool {
        lock(&self.capture).recording.is_some()
    }

    /// Rooms of the explicit recording, if one is running.
    pub fn recording_rooms(&self) -> Option<Vec<String>> {
        lock(&self.capture)
            .recording
            .as_ref()
            .map(|recording| recording.rooms.iter().cloned().collect())
    }

    pub fn recent_buffer_enabled(&self) -> bool {
        lock(&self.control).recent_enabled
    }

    /// Whether the recorder holds any room, so the compositor knows to
    /// measure surfaces for it.
    pub fn is_observing(&self) -> bool {
        !self.subscription.rooms().is_empty()
    }

    /// Ask for a keyframe of the app showing `surface_id` now — the capture
    /// path calls this with each captured frame so structure and pixels line
    /// up. It still waits out [`KEYFRAME_MIN_GAP`]; a surface outside every
    /// held room is not asked.
    pub fn inspect_now(&self, surface_id: u64) {
        if let Some(keyframes) = lock(&self.control).keyframes.as_mut()
            && let Some(app) = keyframes.broker.inspectable_app_for_surface(surface_id)
            && !is_protected(&app.app_id)
        {
            keyframes.triggered.insert(app.peer_id);
        }
    }

    /// Collect answered keyframes and ask for the ones that are due.
    fn pump_keyframes(&self, now: Instant, user: &UserId) {
        let held = self.subscription.rooms();
        let mut control = lock(&self.control);
        let Some(keyframes) = control.keyframes.as_mut() else {
            return;
        };
        let keep = keyframes.collect(now);
        let protected = if held.is_empty() {
            keyframes.forget();
            Vec::new()
        } else {
            keyframes.ask(&held, now, user)
        };
        drop(control);
        if keep.is_empty() && protected.is_empty() {
            return;
        }
        let mut capture = lock(&self.capture);
        for (room, app_id) in &protected {
            capture.note_protected(room, app_id);
        }
        for keyframe in keep {
            let route = CrdtRoute::record(
                keyframe.route.app_id.clone(),
                keyframe.route.room.clone(),
                keyframe.route.record_id.clone(),
            );
            if capture.recent_rooms.contains(&keyframe.route.room)
                && let Some(recent) = capture.recent.as_mut()
            {
                recent.push(now, RecentItem::Inspect(keyframe.clone()));
            }
            if let Some(recording) = capture.recording.as_mut()
                && recording.wants(&route)
            {
                recording.builder.push_inspect(now, keyframe);
            }
        }
    }

    /// Take this frame's measurement of the client surfaces on screen. Only
    /// surfaces tiled in a held room are kept, by the app they belong to.
    pub fn note_surfaces(&self, measured: &[MeasuredSurface]) {
        lock(&self.capture).surface_scales = measured
            .iter()
            .map(|surface| (surface.tab, surface.scale))
            .collect();
        let held = self.subscription.rooms();
        let mut protected: Vec<(String, String)> = Vec::new();
        let surfaces: Vec<(String, SurfaceGeometry)> = {
            let tree = self.tree.read();
            let mut out = Vec::new();
            for workspace in &tree.desktop.workspaces {
                let room = workspace.room.to_string();
                if !held.contains(&room) {
                    continue;
                }
                let tabs = workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs);
                for tab in tabs {
                    let Some(app_id) = tab_app_id(&tab.tile) else {
                        continue;
                    };
                    if is_protected(&app_id) {
                        protected.push((room.clone(), app_id));
                        continue;
                    }
                    if let Some(size) = measured.iter().find(|m| m.tab == tab.id) {
                        out.push((
                            room.clone(),
                            SurfaceGeometry {
                                app_id,
                                width: size.width,
                                height: size.height,
                                scale: size.scale,
                                output: size.output.clone(),
                            },
                        ));
                    }
                }
            }
            drop(tree);
            out
        };
        let mut capture = lock(&self.capture);
        for (room, app_id) in &protected {
            capture.note_protected(room, app_id);
        }
        if capture.surfaces == surfaces {
            return;
        }
        if let Some(recording) = capture.recording.as_mut() {
            for (room, surface) in &surfaces {
                if recording.rooms.contains(room)
                    && (recording.apps.is_empty() || recording.apps.contains(&surface.app_id))
                {
                    recording.builder.note_surface(surface.clone());
                }
            }
        }
        capture.surfaces = surfaces;
    }

    /// Record ids and summaries of the sessions waiting for review.
    pub fn awaiting_review(&self) -> Vec<(RecordCheckout, SessionSummary)> {
        lock(&self.control)
            .awaiting_review
            .iter()
            .map(|pending| (pending.checkout.clone(), pending.summary.clone()))
            .collect()
    }

    /// What `state-dump` reports under `feedback`.
    pub fn status_json(&self) -> serde_json::Value {
        let (recording, finishing) = {
            let capture = lock(&self.capture);
            (
                capture.recording.as_ref().map(|recording| {
                    serde_json::json!({
                        "rooms": recording.rooms,
                        "apps": recording.apps,
                        "events": recording.builder.event_count(),
                        "snapshots": recording.builder.snapshot_count(),
                        "keyframes": recording.builder.keyframe_count(),
                        "truncated": recording.builder.is_truncated(),
                        "pixels": recording.frames.as_ref().map(|frames| frames.purpose().as_str()),
                        "frames": recording.frames.as_ref().map(FrameStripBuilder::frame_count),
                    })
                }),
                capture.finishing.is_some(),
            )
        };
        let control = lock(&self.control);
        serde_json::json!({
            "agent": RECORDER_AGENT,
            "rooms": self.rooms(),
            "recording": recording,
            "capturing": finishing,
            "recent_buffer": control.recent_enabled,
            "awaiting_review": control.awaiting_review.iter().map(|pending| serde_json::json!({
                "record": pending.checkout.id.as_str(),
                "kind": pending.checkout.kind.as_str(),
                "home": launcher::appdir::PERSONAL_ROOM,
                "summary": pending.summary,
                "frames": pending.frames().map(|strip| strip.frames.len()),
            })).collect::<Vec<_>>(),
        })
    }

    /// Read a session waiting for review without taking it: review walks
    /// through it while it stays in memory, and only sending or discarding
    /// takes it.
    pub fn with_pending<T>(
        &self,
        id: &RecordId,
        read: impl FnOnce(&PendingSession) -> T,
    ) -> Option<T> {
        let control = lock(&self.control);
        control
            .awaiting_review
            .iter()
            .find(|pending| &pending.checkout.id == id)
            .map(read)
    }

    /// A copy of the session record `id` waiting for review, if it is one.
    pub fn session_document(&self, id: &RecordId) -> Option<AutoCommit> {
        lock(&self.control)
            .awaiting_review
            .iter()
            .find(|pending| &pending.checkout.id == id)
            .map(|pending| pending.doc.clone())
    }

    /// Keep what replaying a pending session found, so it is checked once
    /// and its layer is the one the session is summarized with. `false`
    /// when no such session awaits review.
    pub fn record_divergence(
        &self,
        id: &RecordId,
        report: session_trace::divergence::DivergenceReport,
    ) -> bool {
        let mut control = lock(&self.control);
        let found = control
            .awaiting_review
            .iter_mut()
            .find(|pending| &pending.checkout.id == id)
            .map(|pending| {
                pending.summary.layer = report.layer;
                pending.divergence = Some(report);
            })
            .is_some();
        drop(control);
        found
    }

    /// Hand a reviewed-or-discarded session over, removing it from memory.
    pub fn take_session(&self, id: &RecordId) -> Option<PendingSession> {
        let mut control = lock(&self.control);
        let index = control
            .awaiting_review
            .iter()
            .position(|pending| &pending.checkout.id == id)?;
        Some(control.awaiting_review.remove(index))
    }

    /// Whether the recorder's own admission in `room` is live: the room's
    /// governance admits it and does not block it, or it acts there as a
    /// delegate on this device that still stands. Another member's
    /// recorder, listed here from their delegation, is not this one.
    fn admitted_in(&self, room: &str) -> bool {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .is_some_and(|workspace| {
                let governed = workspace.capabilities.iter().any(|cap| {
                    cap.id == RECORDER_ADMISSION && cap.is_live_at(Some(tilo_clock::unix_now()))
                }) && workspace.role_of_agent(RECORDER_AGENT)
                    != Some(CapRole::Denied);
                governed || WorkspaceTree::local_delegate_stands(workspace, RECORDER_AGENT)
            })
    }

    /// Admit the recorder to `room` as an Observer. A room `user`
    /// administers, on the machine that governs it, grants it through the
    /// tree so the grant reaches the room document, its members and disk
    /// like any agent admission. Anywhere else the recorder acts as `user`'s
    /// delegate on this device, which needs no administrator: it reads no
    /// more than `user` can ([`WorkspaceTree::admit_device_delegate`]), and
    /// the network announces it so every member lists it. A room whose
    /// administrators turned off recording delegates refuses that. A room
    /// that already admits the recorder is left alone.
    fn admit(&self, control: &mut Control, room: &str, user: &UserId) -> Result<(), RecorderError> {
        if self.admitted_in(room) {
            return Ok(());
        }
        let mut tree = self.tree.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .ok_or_else(|| RecorderError::UnknownRoom(room.to_string()))?;
        let workspace_id = workspace.id;
        let administers = workspace.may_administer(user);
        let forbidden = workspace
            .recording_delegates_forbidden
            .then(|| workspace.name.clone());
        let cap =
            CapOverride::membership(Subject::agent(RECORDER_AGENT), room.to_string(), Verb::Read)
                .with_id(RECORDER_ADMISSION.to_string());
        let granted = administers
            .then(|| tree.grant_in_workspace(workspace_id, user, cap.clone()))
            .flatten();
        let granted = match granted {
            Some(id) => Some(id),
            None if let Some(name) = forbidden => {
                return Err(RecorderError::RecordingNotAllowed(name));
            }
            None => tree.admit_device_delegate(workspace_id, user, cap),
        };
        drop(tree);
        let Some(id) = granted else {
            return Err(RecorderError::NotMember {
                user: user.clone(),
                room: room.to_string(),
            });
        };
        control.granted.insert(room.to_string(), (workspace_id, id));
        Ok(())
    }

    /// Revoke the admission this recorder was given in `room`, if any.
    fn release(&self, control: &mut Control, room: &str, user: &UserId) {
        if let Some((workspace_id, id)) = control.granted.remove(room) {
            let mut tree = self.tree.write();
            let revoked = tree.revoke_device_delegate(workspace_id, &id)
                || tree.revoke_in_workspace(workspace_id, user, &id);
            drop(tree);
            if !revoked {
                tracing::warn!(%room, "recorder: could not revoke its own admission");
            }
        }
    }

    /// Drop device-delegate admissions a previous run left behind — it
    /// ended without stopping — so the recorder is never listed, or
    /// admitted, in a room it does not hold.
    fn drop_stale_delegations(tree: &WorkspaceTreeHandle) {
        let mut tree = tree.write();
        let stale: Vec<(WorkspaceId, String)> = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .capabilities
                    .iter()
                    .filter(|cap| {
                        matches!(&cap.subject, Subject::Principal(Principal::Agent(agent)) if agent == RECORDER_AGENT)
                            && crate::shell::state::is_local_device_delegate(cap)
                    })
                    .map(|cap| (workspace.id, cap.id.clone()))
            })
            .collect();
        for (workspace_id, id) in stale {
            tree.revoke_device_delegate(workspace_id, &id);
        }
    }

    /// Start recording `rooms`, optionally only `apps`' routes in them,
    /// keeping frames of their surfaces when `pixels` says why.
    ///
    /// # Errors
    ///
    /// When a recording is running, no room was named, or the recorder
    /// cannot be admitted to one of them.
    pub fn start(
        &self,
        rooms: &[String],
        apps: Vec<String>,
        pixels: Option<PixelPurpose>,
        user: &UserId,
    ) -> Result<(), RecorderError> {
        if rooms.is_empty() {
            return Err(RecorderError::NoRooms);
        }
        if self.is_recording() {
            return Err(RecorderError::AlreadyRecording);
        }
        let mut control = lock(&self.control);
        let mut admitted: Vec<String> = Vec::new();
        for room in rooms {
            if let Err(error) = self.admit(&mut control, room, user) {
                for room in admitted {
                    if !lock(&self.capture).recent_rooms.contains(&room) {
                        self.release(&mut control, &room, user);
                    }
                }
                return Err(error);
            }
            admitted.push(room.clone());
        }
        drop(control);
        let now = Instant::now();
        let mut builder = TraceBuilder::new(
            session_trace::Origin::Explicit,
            now,
            SystemTime::now(),
            self.limits,
        );
        builder.set_environment(environment());
        {
            let mut capture = lock(&self.capture);
            let mut recording = Recording {
                rooms: rooms.iter().cloned().collect(),
                apps,
                builder,
                frames: pixels
                    .map(|purpose| FrameStripBuilder::new(purpose, FrameLimits::default())),
            };
            // Surfaces already measured, and protected apps already seen,
            // in a room the buffer held.
            for (room, surface) in &capture.surfaces {
                if recording.rooms.contains(room)
                    && (recording.apps.is_empty() || recording.apps.contains(&surface.app_id))
                {
                    recording.builder.note_surface(surface.clone());
                }
            }
            for (room, app_id) in &capture.protected {
                if recording.rooms.contains(room) && recording.wants_app(app_id) {
                    recording.builder.note_protected(app_id, room);
                }
            }
            capture.recording = Some(recording);
        }
        self.resync_rooms();
        for room in rooms {
            // Arrive like any participant: the room's apps restate what they
            // show, and its document holders answer with their documents.
            self.fanout.announce_arrival(&self.subscription, room);
            let asked = self.fanout.request_room_snapshots(room, self.peer_id);
            tracing::debug!(%room, routes = asked.len(), "recorder: asked for starting snapshots");
        }
        Ok(())
    }

    /// Stop the explicit recording and seal it as a `session` record.
    ///
    /// # Errors
    ///
    /// When nothing is recording, or the session cannot be written.
    pub fn stop(&self, user: &UserId) -> Result<RecordCheckout, RecorderError> {
        let recording = lock(&self.capture)
            .recording
            .take()
            .ok_or(RecorderError::NotRecording)?;
        let trace = recording.builder.finish(Instant::now());
        let frames = recording.frames;
        {
            let mut control = lock(&self.control);
            let recent_rooms = lock(&self.capture).recent_rooms.clone();
            for room in &recording.rooms {
                if !recent_rooms.contains(room) {
                    self.release(&mut control, room, user);
                }
            }
        }
        self.resync_rooms();
        self.seal(&trace, frames)
    }

    /// The scale the surface of `tab` renders at, as last measured.
    pub fn surface_scale(&self, tab: crate::shell::state::TabId) -> Option<f64> {
        lock(&self.capture)
            .surface_scales
            .iter()
            .find(|(id, _)| *id == tab)
            .map(|(_, scale)| *scale)
    }

    /// What the compositor should capture, while a recording keeps pixels.
    pub fn pixel_request(&self) -> Option<PixelRequest> {
        let capture = lock(&self.capture);
        let request = capture.recording.as_ref().and_then(|recording| {
            Some(PixelRequest {
                rooms: recording.rooms.iter().cloned().collect(),
                apps: recording.apps.clone(),
                purpose: recording.frames.as_ref()?.purpose(),
            })
        });
        drop(capture);
        request
    }

    /// Keep one captured frame of a recorded surface, taken at `at`.
    /// It is masked with the surface's semantic layout as the trace last
    /// described it before it is encoded; sealing masks again with the
    /// layout around it. Masking and encoding run outside the recorder's
    /// lock, so the broker's fanout never waits on them.
    ///
    /// Returns `None` when no recording keeps pixels of that surface.
    ///
    /// # Errors
    ///
    /// When the frame is malformed or cannot be encoded.
    pub fn push_frame(
        &self,
        at: Instant,
        route: Route,
        width: u32,
        height: u32,
        scale: f64,
        rgba: Vec<u8>,
    ) -> Result<Option<FramePush>, session_trace::frames::FrameError> {
        // A protected app's frames are placeholders; the session's marker
        // stands for them.
        if is_protected(&route.app_id) {
            return Ok(None);
        }
        let capture = lock(&self.capture);
        let wanted = capture
            .recording
            .as_ref()
            .filter(|recording| {
                recording.frames.is_some()
                    && recording.rooms.contains(&route.room)
                    && recording.wants_app(&route.app_id)
            })
            .map(|recording| {
                (
                    recording.builder.offset_at(at),
                    recording.builder.latest_regions(&route),
                )
            });
        drop(capture);
        let Some((offset, layout)) = wanted else {
            return Ok(None);
        };
        let frame = SurfaceFrame::new(offset, route, width, height, scale, rgba)?;
        let prepared = PreparedFrame::prepare(frame, &layout)?;
        let pushed = lock(&self.capture)
            .recording
            .as_mut()
            .and_then(|recording| recording.frames.as_mut())
            .map(|frames| frames.push_prepared(prepared));
        Ok(pushed)
    }

    /// Turn the recent buffer on or off and remember the choice through the
    /// settings service.
    ///
    /// # Errors
    ///
    /// When the setting cannot be saved; the buffer is still switched.
    pub fn set_recent_buffer(&self, enabled: bool, user: &UserId) -> std::io::Result<()> {
        self.apply_recent_buffer(enabled, user);
        let mut values = serde_json::Map::new();
        values.insert(RECENT_BUFFER_KEY.into(), serde_json::Value::Bool(enabled));
        let saved = settings_store::update_section(RECENT_BUFFER_SECTION, values);
        lock(&self.control).settings_stamp = settings_stamp();
        saved
    }

    fn apply_recent_buffer(&self, enabled: bool, user: &UserId) {
        let mut control = lock(&self.control);
        if control.recent_enabled == enabled {
            return;
        }
        control.recent_enabled = enabled;
        control.refused.clear();
        let mut capture = lock(&self.capture);
        if enabled {
            capture.recent = Some(RecentBuffer::default());
            return;
        }
        // Off means forgotten: the ring is dropped, never flushed anywhere.
        capture.recent = None;
        let rooms = std::mem::take(&mut capture.recent_rooms);
        let recorded: BTreeSet<String> = capture
            .recording
            .as_ref()
            .map(|recording| recording.rooms.clone())
            .unwrap_or_default();
        drop(capture);
        for room in rooms.difference(&recorded) {
            self.release(&mut control, room, user);
        }
        drop(control);
        self.resync_rooms();
    }

    /// Build a session from the recent buffer. The session is sealed once
    /// every recorded route answered its end-of-window snapshot request, or
    /// after a short wait; [`Self::pump`] finishes it.
    ///
    /// # Errors
    ///
    /// When the buffer is off or empty, or a capture is still finishing.
    pub fn capture_recent(&self) -> Result<(), RecorderError> {
        let now = Instant::now();
        let mut capture = lock(&self.capture);
        if capture.finishing.is_some() {
            return Err(RecorderError::CaptureInProgress);
        }
        let rooms = capture.recent_rooms.clone();
        let Some(recent) = capture.recent.as_mut() else {
            return Err(RecorderError::RecentBufferOff);
        };
        recent.evict(now);
        if recent.is_empty() {
            return Err(RecorderError::NothingRecent);
        }
        let channels = Arc::clone(&self.channels);
        let opener = self.opener.clone();
        let mut builder = recent.capture(now, SystemTime::now(), self.limits, |reference| {
            resolve_op(&channels, &opener, reference)
        });
        builder.set_environment(environment());
        for (room, surface) in &capture.surfaces {
            if capture.recent_rooms.contains(room) {
                builder.note_surface(surface.clone());
            }
        }
        for (room, app_id) in &capture.protected {
            if capture.recent_rooms.contains(room) {
                builder.note_protected(app_id, room);
            }
        }
        drop(capture);
        let mut awaiting = Vec::new();
        for room in &rooms {
            awaiting.extend(
                self.fanout
                    .request_room_snapshots(room, self.peer_id)
                    .into_iter()
                    .map(|route| trace_route(&route)),
            );
        }
        lock(&self.capture).finishing = Some(Finishing {
            builder,
            awaiting,
            deadline: now.checked_add(CAPTURE_SNAPSHOT_WAIT).unwrap_or(now),
        });
        Ok(())
    }

    /// Frame-loop upkeep: honour revocations, follow the active room with the
    /// recent buffer, keep the roster honest, and finish pending captures.
    /// Returns whether anything visible changed.
    pub fn pump(&self, now: Instant, user: &UserId) -> bool {
        self.reload_recent_setting(user);
        let mut changed = self.honour_revocations();
        changed |= self.follow_active_room(user);
        changed |= self.resync_rooms();
        changed |= self.finish_capture(now);
        self.pump_keyframes(now, user);
        if let Some(recent) = lock(&self.capture).recent.as_mut() {
            recent.evict(now);
        }
        changed
    }

    fn reload_recent_setting(&self, user: &UserId) {
        let stamp = settings_stamp();
        {
            let mut control = lock(&self.control);
            if control.settings_stamp == stamp && stamp.is_some() {
                return;
            }
            control.settings_stamp = stamp;
        }
        let enabled = settings_store::bool_at(
            &settings_store::load(),
            RECENT_BUFFER_SECTION,
            RECENT_BUFFER_KEY,
        )
        .unwrap_or(false);
        self.apply_recent_buffer(enabled, user);
    }

    /// Drop every room whose admission is no longer live. A recording that
    /// loses its last room is sealed with what it has.
    fn honour_revocations(&self) -> bool {
        let (recorded, recent): (Vec<String>, Vec<String>) = {
            let capture = lock(&self.capture);
            (
                capture
                    .recording
                    .as_ref()
                    .map(|recording| recording.rooms.iter().cloned().collect())
                    .unwrap_or_default(),
                capture.recent_rooms.iter().cloned().collect(),
            )
        };
        let revoked: BTreeSet<String> = recorded
            .iter()
            .chain(&recent)
            .filter(|room| !self.admitted_in(room))
            .cloned()
            .collect();
        if revoked.is_empty() {
            return false;
        }
        let mut control = lock(&self.control);
        for room in &revoked {
            // Revoked by someone else, or a delegate that no longer stands
            // (the room turned delegates off, its person lost their role):
            // take back what is left of ours, so it is not announced again.
            if let Some((workspace_id, id)) = control.granted.remove(room) {
                self.tree.write().revoke_device_delegate(workspace_id, &id);
            }
            if recent.contains(room) {
                control.refused.insert(room.clone());
            }
        }
        drop(control);
        let sealed = {
            let mut capture = lock(&self.capture);
            capture.recent_rooms.retain(|room| !revoked.contains(room));
            let emptied = capture.recording.as_mut().is_some_and(|recording| {
                recording.rooms.retain(|room| !revoked.contains(room));
                recording.rooms.is_empty()
            });
            if emptied {
                capture.recording.take()
            } else {
                None
            }
        };
        if let Some(recording) = sealed
            && let Err(error) =
                self.seal(&recording.builder.finish(Instant::now()), recording.frames)
        {
            tracing::warn!(%error, "recorder: a revoked recording could not be sealed");
        }
        self.resync_rooms();
        true
    }

    fn follow_active_room(&self, user: &UserId) -> bool {
        let mut control = lock(&self.control);
        if !control.recent_enabled {
            return false;
        }
        let Some(room) = self
            .tree
            .read()
            .desktop
            .active_workspace()
            .filter(|workspace| !workspace.room.is_nil())
            .map(|workspace| workspace.room.to_string())
        else {
            return false;
        };
        if control.refused.contains(&room) || lock(&self.capture).recent_rooms.contains(&room) {
            return false;
        }
        if let Err(error) = self.admit(&mut control, &room, user) {
            tracing::debug!(%error, "recorder: the recent buffer cannot follow this room");
            control.refused.insert(room);
            return false;
        }
        drop(control);
        lock(&self.capture).recent_rooms.insert(room);
        true
    }

    /// Point the subscription at exactly the rooms held. Holding a room is
    /// what lists the recorder there: the roster reads the broker's live
    /// participants ([`crate::shell::peer_presence::LocalParticipantRoster`]).
    fn resync_rooms(&self) -> bool {
        let held: BTreeSet<String> = {
            let capture = lock(&self.capture);
            capture
                .recording
                .iter()
                .flat_map(|recording| recording.rooms.iter().cloned())
                .chain(capture.recent_rooms.iter().cloned())
                .collect()
        };
        let held: BTreeSet<String> = held
            .into_iter()
            .filter(|room| self.admitted_in(room))
            .collect();
        self.subscription.set_rooms(held.iter().cloned().collect());
        lock(&self.capture)
            .protected
            .retain(|(room, _)| held.contains(room));
        let mut control = lock(&self.control);
        if control.held == held {
            return false;
        }
        for room in held.difference(&control.held) {
            self.fanout.announce_arrival(&self.subscription, room);
        }
        control.held = held;
        true
    }

    /// Seal a retroactive capture once its snapshots arrived, its wait ran
    /// out.
    fn finish_capture(&self, now: Instant) -> bool {
        let finishing = {
            let mut capture = lock(&self.capture);
            let ready = capture.finishing.as_ref().is_some_and(|finishing| {
                now >= finishing.deadline
                    || finishing
                        .awaiting
                        .iter()
                        .all(|route| finishing.builder.has_snapshot(route))
            });
            if !ready {
                return false;
            }
            capture.finishing.take()
        };
        let Some(finishing) = finishing else {
            return false;
        };
        if let Err(error) = self.seal(&finishing.builder.finish(now), None) {
            tracing::warn!(%error, "recorder: the captured session could not be sealed");
        }
        true
    }

    /// Wrap `trace` in a `session` record homed in the personal room and
    /// keep it for review. Kept frames are sealed against the finished
    /// trace and referenced as the record's `frames` blob.
    fn seal(
        &self,
        trace: &SessionTrace,
        frames: Option<FrameStripBuilder>,
    ) -> Result<RecordCheckout, RecorderError> {
        let checkout = RecordCheckout::new(
            RecordId::minted(),
            RecordKind::new(session_trace::SESSION_RECORD_KIND),
        );
        let mut doc = AutoCommit::new();
        ui::record::init(
            &mut doc,
            &checkout.id,
            &checkout.kind,
            launcher::appdir::PERSONAL_ROOM,
        );
        for route in trace.routes() {
            if let Some(recorded) = RecordId::parse(&route.record_id) {
                ui::record::add_reference(
                    &mut doc,
                    &RecordRef::new(recorded, session_trace::RECORDED_RELATION),
                );
            }
        }
        let frames = frames
            .map(|frames| seal_frames(&mut doc, trace, frames))
            .transpose()?;
        session_trace::write(&mut doc, trace)
            .map_err(|error| RecorderError::Write(error.to_string()))?;
        let summary = trace.summary();
        let mut control = lock(&self.control);
        if control.awaiting_review.len() >= MAX_AWAITING_REVIEW {
            control.awaiting_review.remove(0);
        }
        control.awaiting_review.push(PendingSession {
            checkout: checkout.clone(),
            summary,
            doc,
            frames,
            divergence: None,
        });
        drop(control);
        Ok(checkout)
    }
}

/// Seal a frame strip against `trace` and reference its bytes from `doc`.
fn seal_frames(
    doc: &mut AutoCommit,
    trace: &SessionTrace,
    frames: FrameStripBuilder,
) -> Result<SealedFrames, RecorderError> {
    let strip = frames
        .finish(trace)
        .map_err(|error| RecorderError::Write(error.to_string()))?;
    let blob = strip
        .encode()
        .map_err(|error| RecorderError::Write(error.to_string()))?;
    let reference = ui::record::BlobRef {
        hash: blake3::hash(&blob).to_hex().to_string(),
        len: u64::try_from(blob.len()).unwrap_or(u64::MAX),
        mime: session_trace::frames::FRAMES_MIME.to_string(),
        relation: session_trace::FRAMES_BLOB_RELATION.to_string(),
    };
    if !ui::record::add_blob_reference(doc, &reference) {
        return Err(RecorderError::Write(
            "the frame blob could not be referenced".into(),
        ));
    }
    Ok(SealedFrames { strip, blob })
}

fn environment() -> Environment {
    let settings = settings_store::load();
    Environment {
        build: format!("tilo-shell {}", env!("CARGO_PKG_VERSION")),
        image: image_identity(),
        surfaces: Vec::new(),
        theme: settings_store::str_at(&settings, "appearance", "theme")
            .unwrap_or("system")
            .to_string(),
        locale: std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default(),
        protected: Vec::new(),
    }
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("rooms", &self.rooms())
            .field("recording", &self.is_recording())
            .finish_non_exhaustive()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.fanout.unsubscribe(&self.subscription);
    }
}

fn settings_stamp() -> Option<SystemTime> {
    settings_store::settings_path().and_then(|path| std::fs::metadata(path).ok()?.modified().ok())
}

/// The image identity the booted system declares about itself.
fn image_identity() -> Option<ImageIdentity> {
    let release = std::fs::read_to_string("/usr/lib/os-release").ok()?;
    let field = |key: &str| {
        release.lines().find_map(|line| {
            line.strip_prefix(key)?
                .strip_prefix('=')
                .map(|value| value.trim_matches('"').to_string())
        })
    };
    Some(ImageIdentity {
        name: field("NAME").unwrap_or_default(),
        version: field("IMAGE_VERSION")
            .or_else(|| field("VERSION_ID"))
            .unwrap_or_default(),
        generation: std::fs::read_to_string("/usr/lib/tilo/generation")
            .ok()
            .and_then(|text| text.trim().parse().ok()),
        digest: IMAGE_DIGEST.get().cloned().unwrap_or(ImageDigest::Unknown),
    })
}

/// The booted image digest, read once per session off the frame thread.
static IMAGE_DIGEST: std::sync::OnceLock<ImageDigest> = std::sync::OnceLock::new();

/// Start reading the booted image digest in the background; recordings made
/// before it answers say `Unknown`.
fn warm_image_digest() {
    if IMAGE_DIGEST.get().is_some() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tilo-recorder-image".into())
        .spawn(|| {
            let _ = IMAGE_DIGEST.set(booted_image_digest());
        });
    if let Err(error) = spawned {
        tracing::debug!(%error, "recorder: could not read the booted image");
    }
}

/// What bootc says is booted. A host without `/run/ostree-booted` is not a
/// bootc system (a development or nested host) and says so.
fn booted_image_digest() -> ImageDigest {
    if !std::path::Path::new("/run/ostree-booted").exists() {
        return ImageDigest::NotBootc;
    }
    let output = match std::process::Command::new("bootc")
        .args(["status", "--format", "json"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(output) => {
            return ImageDigest::Unavailable(format!("bootc status exited with {}", output.status));
        }
        Err(error) => return ImageDigest::Unavailable(format!("bootc status: {error}")),
    };
    digest_from_bootc_status(&output)
}

fn digest_from_bootc_status(json: &[u8]) -> ImageDigest {
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(json) else {
        return ImageDigest::Unavailable("bootc status is not JSON".into());
    };
    status
        .pointer("/status/booted/image/imageDigest")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || ImageDigest::Unavailable("bootc reports no booted image digest".into()),
            |digest| ImageDigest::Booted(digest.to_string()),
        )
}

fn trace_route(route: &CrdtRoute) -> Route {
    Route::new(
        route.app_id.clone(),
        route.cluster_id.clone(),
        route.record_id.clone(),
    )
}

/// Serve a referenced op from the broker's validated history, opened as the
/// recording keeps it. Redacted ops come back as their redaction marker, as
/// they would to any reader.
fn resolve_op(
    channels: &Mutex<BrokerChannels>,
    opener: &crate::shell::participant_keys::ChannelOpener,
    reference: &OpReference,
) -> Option<collab_types::SignedOp> {
    let (page, _) =
        lock(channels).scrollback_page(reference.artifact, reference.seq.saturating_add(1), 64);
    let op = page.into_iter().find(|op| reference.names(op))?;
    let route = CrdtRoute {
        app_id: reference.route.app_id.clone(),
        cluster_id: reference.route.room.clone(),
        record_id: reference.route.record_id.clone(),
    };
    Some(opener.opened(&route, &op).unwrap_or(op))
}

/// A document event opened for the recording: what was said on an
/// encrypted route, kept on this device like everything else it records
/// until review redacts it. `None` when there is nothing to open, or it
/// could not be opened (the recording then keeps what arrived).
fn opened_event(
    opener: &crate::shell::participant_keys::ChannelOpener,
    event: &CollabEvent,
    route: &CrdtRoute,
) -> Option<CollabEvent> {
    let CollabEvent::Document {
        envelope,
        from_user_id,
        ..
    } = event
    else {
        return None;
    };
    envelope.encryption?;
    match opener.opened(route, envelope) {
        Ok(clear) => Some(CollabEvent::Document {
            envelope: Box::new(clear),
            from_user_id: from_user_id.clone(),
            decrypted_payload: None,
        }),
        Err(error) => {
            tracing::debug!(%error, "recorder: a recorded op could not be opened");
            None
        }
    }
}

fn presence_change(key: &PresenceKey, payload: &PresencePayload) -> PresenceChange {
    let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    // A View subset rides inside its key with an inert payload; the trace
    // names it so a reader finds view changes without decoding every scope.
    if matches!(key, PresenceKey::View { .. }) {
        return PresenceChange::View;
    }
    match payload {
        PresencePayload::Pointer { x, y } => PresenceChange::Pointer { x: *x, y: *y },
        PresencePayload::Caret { pos } => PresenceChange::Caret { pos: wide(*pos) },
        PresencePayload::Selection { start, end } => PresenceChange::Selection {
            start: wide(*start),
            end: wide(*end),
        },
        PresencePayload::Typing { start, end } => PresenceChange::Typing {
            start: wide(*start),
            end: wide(*end),
        },
        PresencePayload::Arrived => PresenceChange::Arrived,
        PresencePayload::Left => PresenceChange::Left,
        PresencePayload::Following { target } => PresenceChange::Following {
            target: target.clone(),
        },
        PresencePayload::Interaction(update) => PresenceChange::Interaction(update.clone()),
        PresencePayload::Status { text, automatic } => PresenceChange::Status {
            text: text.clone(),
            automatic: *automatic,
        },
    }
}

fn presence_update(
    participant: &str,
    route: &CrdtRoute,
    key: &PresenceKey,
    payload: &PresencePayload,
) -> Option<PresenceUpdate> {
    Some(PresenceUpdate {
        participant: participant.to_string(),
        room: route.cluster_id.clone(),
        scope: postcard::to_allocvec(key).ok()?,
        change: presence_change(key, payload),
    })
}

/// The sink: keep what was delivered, where it was asked for. Nothing a
/// protected app sends is kept — not its snapshot, ops or presence — only
/// that it was there.
fn observe(capture: &mut Capture, now: Instant, event: &CollabEvent, route: &CrdtRoute) {
    let delivered = matches!(
        event,
        CollabEvent::Document { .. }
            | CollabEvent::RemotePresenceUpdate { .. }
            | CollabEvent::RemoteCursorMoved { .. }
    );
    if delivered && is_protected(&route.app_id) {
        capture.note_protected(&route.cluster_id, &route.app_id);
        return;
    }
    match event {
        CollabEvent::Document {
            envelope,
            from_user_id,
            ..
        } => {
            let traced = trace_route(route);
            if let Some(recording) = capture.recording.as_mut()
                && recording.wants(route)
                && recording.builder.push_op(
                    now,
                    traced.clone(),
                    from_user_id.clone(),
                    (**envelope).clone(),
                ) == Pushed::Full
            {
                tracing::debug!("recorder: the recording is full; later events are not kept");
            }
            if let Some(finishing) = capture.finishing.as_mut()
                && finishing.awaiting.contains(&traced)
                && !finishing.builder.has_snapshot(&traced)
            {
                finishing.builder.push_op(
                    now,
                    traced.clone(),
                    from_user_id.clone(),
                    (**envelope).clone(),
                );
            }
            if capture.recent_rooms.contains(&route.cluster_id)
                && let Some(recent) = capture.recent.as_mut()
            {
                recent.push(
                    now,
                    RecentItem::Op(OpReference::of(traced, from_user_id.clone(), envelope)),
                );
            }
        }
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } => record_presence(capture, now, user_id, route, key, payload),
        CollabEvent::RemoteCursorMoved { user_id, x, y } => record_presence(
            capture,
            now,
            user_id,
            route,
            &PresenceKey::pointer_default(),
            &PresencePayload::Pointer { x: *x, y: *y },
        ),
        _ => {}
    }
}

fn record_presence(
    capture: &mut Capture,
    now: Instant,
    user: &str,
    route: &CrdtRoute,
    key: &PresenceKey,
    payload: &PresencePayload,
) {
    let recorded = capture
        .recording
        .as_ref()
        .is_some_and(|recording| recording.rooms.contains(&route.cluster_id));
    let buffered = capture.recent.is_some() && capture.recent_rooms.contains(&route.cluster_id);
    if !recorded && !buffered {
        return;
    }
    let Some(update) = presence_update(user, route, key, payload) else {
        return;
    };
    if buffered && let Some(recent) = capture.recent.as_mut() {
        recent.push(now, RecentItem::Presence(update.clone()));
    }
    if recorded && let Some(recording) = capture.recording.as_mut() {
        recording.builder.push_presence(now, update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_follow_reported_changes_debounced_and_poll_only_silent_apps() {
        let ms = Duration::from_millis;
        assert!(
            keyframe_due(0, None, Duration::MAX, false),
            "the first keyframe"
        );
        assert!(
            !keyframe_due(3, Some(3), ms(5_000), false),
            "an unchanged tree is not asked again"
        );
        assert!(
            keyframe_due(4, Some(3), ms(300), false),
            "a reported change is asked"
        );
        assert!(
            !keyframe_due(4, Some(3), ms(100), false),
            "a burst waits out the gap"
        );
        assert!(
            keyframe_due(3, Some(3), ms(300), true),
            "a captured frame asks"
        );
        assert!(
            !keyframe_due(3, Some(3), ms(100), true),
            "a captured frame still waits"
        );
        assert!(
            !keyframe_due(0, Some(0), ms(1_000), false),
            "a silent app waits for the poll"
        );
        assert!(
            keyframe_due(0, Some(0), ms(2_000), false),
            "a silent app is polled"
        );
    }

    #[test]
    fn the_booted_digest_is_read_from_bootc_status() {
        let status = br#"{"status":{"booted":{"image":{"imageDigest":"sha256:0123"}}}}"#;
        assert_eq!(
            digest_from_bootc_status(status),
            ImageDigest::Booted("sha256:0123".into())
        );
        assert!(matches!(
            digest_from_bootc_status(br#"{"status":{"booted":null}}"#),
            ImageDigest::Unavailable(_)
        ));
        assert!(matches!(
            digest_from_bootc_status(b"not json"),
            ImageDigest::Unavailable(_)
        ));
    }

    #[test]
    fn every_presence_payload_is_kept_in_trace_terms() {
        let route = CrdtRoute::cluster("room-1");
        let key = PresenceKey::Widget { path: Vec::new() };
        for (payload, change) in [
            (
                PresencePayload::Caret { pos: 3 },
                PresenceChange::Caret { pos: 3 },
            ),
            (
                PresencePayload::Selection { start: 1, end: 4 },
                PresenceChange::Selection { start: 1, end: 4 },
            ),
            (PresencePayload::Arrived, PresenceChange::Arrived),
            (
                PresencePayload::Status {
                    text: "fixing".into(),
                    automatic: false,
                },
                PresenceChange::Status {
                    text: "fixing".into(),
                    automatic: false,
                },
            ),
        ] {
            let update = presence_update("alice", &route, &key, &payload).expect("update");
            assert_eq!(update.change, change);
            assert_eq!(update.room, "room-1");
            assert_eq!(
                postcard::from_bytes::<PresenceKey>(&update.scope).expect("scope"),
                key,
                "the scope is the SDK key's own wire encoding"
            );
        }
        let view = PresenceKey::view(&ui::view::ViewSubset::default());
        let update =
            presence_update("alice", &route, &view, &PresencePayload::Arrived).expect("update");
        assert_eq!(
            update.change,
            PresenceChange::View,
            "a View subset is recorded as a view change, its subset in the scope"
        );
        assert!(
            postcard::from_bytes::<PresenceKey>(&update.scope)
                .expect("scope")
                .as_view()
                .is_some()
        );
    }

    #[test]
    fn a_recording_keeps_only_its_rooms_and_apps() {
        let now = Instant::now();
        let mut capture = Capture {
            recording: Some(Recording {
                rooms: BTreeSet::from(["room-1".to_string()]),
                apps: vec!["so.tilo.notes".into()],
                builder: TraceBuilder::new(
                    session_trace::Origin::Explicit,
                    now,
                    SystemTime::now(),
                    Limits::default(),
                ),
                frames: None,
            }),
            ..Capture::default()
        };
        let presence = CollabEvent::RemotePresenceUpdate {
            user_id: "alice".into(),
            key: PresenceKey::Widget { path: Vec::new() },
            payload: PresencePayload::Caret { pos: 1 },
        };
        observe(&mut capture, now, &presence, &CrdtRoute::cluster("room-1"));
        observe(&mut capture, now, &presence, &CrdtRoute::cluster("room-2"));
        let recording = capture.recording.take().expect("recording");
        assert!(recording.wants(&CrdtRoute::record("so.tilo.notes", "room-1", "")));
        assert!(!recording.wants(&CrdtRoute::record("so.tilo.mail", "room-1", "")));
        assert!(!recording.wants(&CrdtRoute::record("so.tilo.notes", "room-2", "")));
        assert_eq!(recording.builder.event_count(), 1);
    }
}
