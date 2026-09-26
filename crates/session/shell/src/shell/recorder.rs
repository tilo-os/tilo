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
//! With the recent buffer on (`feedback.recent_buffer`, off by default), the
//! recorder also follows the rooms the user administers as they visit them
//! and keeps the last two minutes of presence and op references in memory,
//! from which [`Recorder::capture_recent`] builds a session.

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use automerge::AutoCommit;
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
    CapOverride, CapRole, Subject, UserId, Verb, WorkspaceId, WorkspaceTreeHandle,
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
/// A keyframe is asked of each recorded app at least this often, and kept
/// only if the tree changed.
const KEYFRAME_POLL: Duration = Duration::from_secs(2);
/// A triggered keyframe waits at least this long after the last one.
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
    #[error("{user} does not administer room {room}, so the recorder cannot be admitted")]
    NotAdministrator { user: String, room: String },
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

/// One client surface as the compositor measured it this frame.
#[derive(Clone, Debug, PartialEq)]
pub struct MeasuredSurface {
    pub surface_id: u64,
    /// Logical size of its tile.
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub output: String,
}

/// A finished session waiting for review. Held only in memory.
pub struct PendingSession {
    pub checkout: RecordCheckout,
    pub summary: SessionSummary,
    doc: AutoCommit,
}

impl PendingSession {
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
    /// Rooms (and optionally the one app in them) whose semantic tree
    /// likely changed: a View or overlay moved, an action finished, a frame
    /// was captured. Their keyframe is asked for sooner.
    keyframe_triggers: BTreeSet<(String, Option<String>)>,
}

struct Recording {
    rooms: BTreeSet<String>,
    /// Only these apps' routes, or every route when empty.
    apps: Vec<String>,
    builder: TraceBuilder,
}

impl Recording {
    fn wants(&self, route: &CrdtRoute) -> bool {
        self.rooms.contains(&route.cluster_id)
            && (self.apps.is_empty() || self.apps.contains(&route.app_id))
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
    /// Admissions this recorder granted, by room, so it revokes only its own.
    granted: BTreeMap<String, WorkspaceId>,
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

/// Semantic-tree keyframes: asked of each inspectable app in a held room,
/// kept only when the tree changed, on a bounded cadence.
struct Keyframes {
    broker: crate::shell::collab_broker::AppActionBroker,
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

    fn forget(&mut self) {
        self.asked.clear();
        self.last.clear();
        self.kept = 0;
    }

    /// Ask each inspectable app in `held` whose keyframe is due: every
    /// [`KEYFRAME_POLL`], or sooner when a trigger names its room.
    fn ask(
        &mut self,
        held: &[String],
        triggers: &BTreeSet<(String, Option<String>)>,
        now: Instant,
        user: &UserId,
    ) {
        let caller = crate::shell::collab_broker_capnp::InspectCaller {
            person: user.clone(),
            audience: ui::a11y::Audience::LocalSession,
        };
        for app in self.broker.inspectable_apps() {
            if !held.contains(&app.room) || self.kept >= MAX_KEYFRAMES {
                continue;
            }
            let route = Route::new(app.app_id.clone(), app.room.clone(), app.record.clone());
            if self.pending.iter().any(|pending| pending.route == route) {
                continue;
            }
            let since = self
                .asked
                .get(&route)
                .map_or(Duration::MAX, |asked| now.saturating_duration_since(*asked));
            let triggered = triggers.iter().any(|(room, app_id)| {
                *room == app.room && app_id.as_ref().is_none_or(|id| *id == app.app_id)
            });
            if since < KEYFRAME_POLL && !(triggered && since >= KEYFRAME_MIN_GAP) {
                continue;
            }
            self.asked.insert(route.clone(), now);
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
    }
}

/// The session recorder. One per session; shared by the frame loop and the
/// shell action catalogue.
pub struct Recorder {
    tree: WorkspaceTreeHandle,
    fanout: Arc<FanoutState>,
    channels: Arc<Mutex<BrokerChannels>>,
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
        let fanout = broker.fanout();
        let capture: Arc<Mutex<Capture>> = Arc::new(Mutex::new(Capture::default()));
        let sink_capture = Arc::clone(&capture);
        let subscription = fanout.subscribe_rooms_as(
            LocalParticipant {
                agent: RECORDER_AGENT.into(),
                label: RECORDER_LABEL.into(),
            },
            Vec::new(),
            Box::new(move |event, route| {
                observe(&mut lock(&sink_capture), Instant::now(), event, route);
            }),
        );
        let peer_id = fanout.next_peer_id();
        Arc::new(Self {
            tree,
            fanout,
            channels: broker.channels(),
            peer_id,
            capture,
            subscription,
            control: Mutex::new(Control {
                keyframes: broker.app_action_broker().map(|broker| Keyframes {
                    broker,
                    pending: Vec::new(),
                    asked: BTreeMap::new(),
                    last: BTreeMap::new(),
                    kept: 0,
                }),
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
    /// up. A surface outside every held room is ignored.
    pub fn inspect_now(&self, surface_id: u64) {
        let target = {
            let tree = self.tree.read();
            tree.desktop.workspaces.iter().find_map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .flat_map(|strip| &strip.window_groups)
                    .flat_map(|group| &group.tabs)
                    .find_map(|tab| match &tab.tile {
                        crate::shell::state::TileContent::Foreign {
                            app_id,
                            surface_id: id,
                        } if *id == surface_id => {
                            Some((workspace.room.to_string(), Some(app_id.clone())))
                        }
                        _ => None,
                    })
            })
        };
        if let Some(target) = target {
            lock(&self.capture).keyframe_triggers.insert(target);
        }
    }

    /// Collect answered keyframes and ask for the ones that are due.
    fn pump_keyframes(&self, now: Instant, user: &UserId) {
        let held = self.subscription.rooms();
        let triggers = std::mem::take(&mut lock(&self.capture).keyframe_triggers);
        let mut control = lock(&self.control);
        let Some(keyframes) = control.keyframes.as_mut() else {
            return;
        };
        let keep = keyframes.collect(now);
        if held.is_empty() {
            keyframes.forget();
        } else {
            keyframes.ask(&held, &triggers, now, user);
        }
        drop(control);
        if keep.is_empty() {
            return;
        }
        let mut capture = lock(&self.capture);
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
        let held = self.subscription.rooms();
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
                    let crate::shell::state::TileContent::Foreign { app_id, surface_id } =
                        &tab.tile
                    else {
                        continue;
                    };
                    if let Some(size) = measured.iter().find(|m| m.surface_id == *surface_id) {
                        out.push((
                            room.clone(),
                            SurfaceGeometry {
                                app_id: app_id.clone(),
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
                        "truncated": recording.builder.is_truncated(),
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
            })).collect::<Vec<_>>(),
        })
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

    /// Whether the recorder's agent admission in `room` is live.
    fn admitted_in(&self, room: &str) -> bool {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .and_then(|workspace| workspace.role_of_agent(RECORDER_AGENT))
            .is_some_and(|role| role != CapRole::Denied)
    }

    /// Admit the recorder to `room` as an Observer, through the tree so the
    /// grant reaches the room document, its members and disk like any
    /// agent admission. A room that already admits it is left alone.
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
        if !workspace.may_administer(user) {
            return Err(RecorderError::NotAdministrator {
                user: user.clone(),
                room: room.to_string(),
            });
        }
        let cap =
            CapOverride::membership(Subject::agent(RECORDER_AGENT), room.to_string(), Verb::Read)
                .with_id(RECORDER_ADMISSION.to_string());
        let granted = tree.grant_in_workspace(workspace_id, user, cap);
        drop(tree);
        if granted.is_none() {
            return Err(RecorderError::NotAdministrator {
                user: user.clone(),
                room: room.to_string(),
            });
        }
        control.granted.insert(room.to_string(), workspace_id);
        Ok(())
    }

    /// Revoke the admission this recorder granted in `room`, if any.
    fn release(&self, control: &mut Control, room: &str, user: &UserId) {
        if let Some(workspace_id) = control.granted.remove(room) {
            let revoked =
                self.tree
                    .write()
                    .revoke_in_workspace(workspace_id, user, RECORDER_ADMISSION);
            if !revoked {
                tracing::warn!(%room, "recorder: could not revoke its own admission");
            }
        }
    }

    /// Start recording `rooms`, optionally only `apps`' routes in them.
    ///
    /// # Errors
    ///
    /// When a recording is running, no room was named, or the recorder
    /// cannot be admitted to one of them.
    pub fn start(
        &self,
        rooms: &[String],
        apps: Vec<String>,
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
            };
            // Surfaces already measured for a room the buffer held.
            for (room, surface) in &capture.surfaces {
                if recording.rooms.contains(room)
                    && (recording.apps.is_empty() || recording.apps.contains(&surface.app_id))
                {
                    recording.builder.note_surface(surface.clone());
                }
            }
            capture.recording = Some(recording);
        }
        self.resync_rooms();
        for room in rooms {
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
        self.seal(&trace)
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
        let mut builder = recent.capture(now, SystemTime::now(), self.limits, |reference| {
            resolve_op(&channels, reference)
        });
        builder.set_environment(environment());
        for (room, surface) in &capture.surfaces {
            if capture.recent_rooms.contains(room) {
                builder.note_surface(surface.clone());
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
            // Revoked by someone else: nothing of ours is left to revoke.
            control.granted.remove(room);
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
            && let Err(error) = self.seal(&recording.builder.finish(Instant::now()))
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
            .filter(|workspace| !workspace.room.is_nil() && workspace.may_administer(user))
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
        let mut control = lock(&self.control);
        if control.held == held {
            return false;
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
        if let Err(error) = self.seal(&finishing.builder.finish(now)) {
            tracing::warn!(%error, "recorder: the captured session could not be sealed");
        }
        true
    }

    /// Wrap `trace` in a `session` record homed in the personal room and
    /// keep it for review.
    fn seal(&self, trace: &SessionTrace) -> Result<RecordCheckout, RecorderError> {
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
        });
        drop(control);
        Ok(checkout)
    }
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

/// Serve a referenced op from the broker's validated history. Redacted ops
/// come back as their redaction marker, as they would to any reader.
fn resolve_op(
    channels: &Mutex<BrokerChannels>,
    reference: &OpReference,
) -> Option<collab_types::SignedOp> {
    let (page, _) =
        lock(channels).scrollback_page(reference.artifact, reference.seq.saturating_add(1), 64);
    page.into_iter().find(|op| reference.names(op))
}

fn presence_change(payload: &PresencePayload) -> PresenceChange {
    let wide = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
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
        change: presence_change(payload),
    })
}

/// The sink: keep what was delivered, where it was asked for.
fn observe(capture: &mut Capture, now: Instant, event: &CollabEvent, route: &CrdtRoute) {
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
    let reshaped = matches!(key, PresenceKey::View { .. })
        || matches!(payload, PresencePayload::Interaction(update) if update.phase.is_terminal());
    if reshaped {
        capture
            .keyframe_triggers
            .insert((route.cluster_id.clone(), None));
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
