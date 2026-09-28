//! Replay is following a past session (`docs/architecture/feedback.md`).
//!
//! Replaying a `session` record opens an ephemeral **replay room**: a room
//! like any other, marked [`Workspace::ephemeral`] so it is never written,
//! synced or offered, and torn down when the replay closes. The recorded
//! apps are launched into it through the ordinary launcher and sandbox. A
//! [`ReplayPeer`] admitted to the room publishes the recorded documents,
//! changes and presence as the timeline reaches them, and the viewer's apps
//! follow the recorded participant with `FollowMode::Replay`: replay is the
//! production follow path, fed from a record instead of the network.
//!
//! [`ReplayController`] owns the one replay of this session and its
//! transport: play, pause, step, seek and speed, all shell actions. A seek
//! backwards cannot rewind a document that already merged later changes, so
//! it reopens the room around the same windows: the apps are started again
//! in a fresh room, ask the peer for the document, and receive it as it was
//! at the target.
//!
//! A capture-protected app contributed only a marker to the session: it is
//! not launched, and the replay room shows the capture placeholder for it.

pub mod headless;
pub mod offscreen;
pub mod peer;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use session_trace::SessionTrace;
use session_trace::replay::{Playhead, Seek, Speed, Step, StepKind};
use ui::record::RecordId;

pub use peer::{REPLAY_ADMISSION, REPLAY_AGENT, ReplayError, ReplayPeer};

use crate::shell::collab_broker::{BrokerHandle, FanoutState};
use crate::shell::state::WorkspaceAuthority as _;
use crate::shell::state::{
    CapOverride, Collaborator, RoomId, Subject, UserId, Verb, WorkspaceId, WorkspaceTreeHandle,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the person watches the replay, or it runs out of sight (the
/// divergence check replays a trace before it is sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Presentation {
    Shown,
    Offscreen,
}

/// Why a transport request was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("nothing is being replayed")]
    NotReplaying,
    #[error("no session record {0} is reachable from this device")]
    UnknownRecord(String),
    #[error("record {record} is not a readable session: {reason}")]
    NotASession { record: String, reason: String },
    #[error("there is no step {0}")]
    NoSuchStep(usize),
    #[error("already at the last step")]
    AtEnd,
    #[error("already at the first step")]
    AtStart,
    #[error("the replay room could not be opened: {0}")]
    Room(String),
    #[error(transparent)]
    Replay(#[from] ReplayError),
}

/// What the frame loop must do after a transport change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launches {
    /// The replay room apps are launched into.
    pub room: Option<RoomId>,
    pub workspace: Option<WorkspaceId>,
    /// Recorded apps to start there.
    pub apps: Vec<String>,
    /// The room each recorded app was recorded in: the records it is shown,
    /// read-only, in the replay room.
    pub recorded_rooms: Vec<(String, RoomId)>,
    /// Tabs a restarted app's window goes back into, by app id: the window
    /// the person was watching keeps its place.
    pub into_tabs: Vec<(String, crate::shell::state::TabId)>,
    /// Windows of apps that ran in a room the replay left.
    pub close_surfaces: Vec<u64>,
    /// Replay rooms left behind, whose app storage is to be removed.
    pub closed: Vec<RoomId>,
}

/// The apps `trace` recorded, each with the room it was recorded in.
fn recorded_rooms(trace: &SessionTrace) -> Vec<(String, RoomId)> {
    let mut out: Vec<(String, RoomId)> = trace
        .routes()
        .into_iter()
        .filter(|route| !route.app_id.is_empty())
        .filter_map(|route| Some((route.app_id, RoomId::parse(&route.room)?)))
        .collect();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// The capture-protected apps `trace` marks as present, each once. Nothing
/// of them was recorded, so none is launched; the replay room shows the
/// capture placeholder for each instead.
fn protected_apps(trace: &SessionTrace) -> Vec<String> {
    let mut apps: Vec<String> = trace
        .environment
        .protected
        .iter()
        .map(|marker| marker.app_id.clone())
        .collect();
    apps.sort();
    apps.dedup();
    apps
}

/// The app windows open in `workspace`: `(tab, app id, surface)`.
fn app_windows(
    tree: &crate::shell::state::WorkspaceTree,
    workspace: WorkspaceId,
) -> Vec<(crate::shell::state::TabId, String, u64)> {
    tree.desktop
        .workspaces
        .iter()
        .filter(|candidate| candidate.id == workspace)
        .flat_map(|candidate| candidate.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            crate::shell::state::TileContent::Foreign { app_id, surface_id } => {
                Some((tab.id, app_id.clone(), *surface_id))
            }
            _ => None,
        })
        .collect()
}

struct Replay {
    record: Option<RecordId>,
    trace: Arc<SessionTrace>,
    steps: Vec<Step>,
    playhead: Playhead,
    workspace: WorkspaceId,
    room: RoomId,
    owner: UserId,
    peer: Arc<ReplayPeer>,
    apps: Vec<String>,
    presentation: Presentation,
    /// The workspace the person was in, restored when the replay closes.
    return_to: Option<WorkspaceId>,
    last_tick: Option<Instant>,
}

/// The session's replay and its transport. One per session, shared by the
/// frame loop, the shell action catalogue and the broker.
pub struct ReplayController {
    tree: WorkspaceTreeHandle,
    fanout: Arc<FanoutState>,
    /// The content keys the replay peer speaks with, and the channel
    /// service a closed replay room's channels are forgotten from.
    keys: crate::shell::participant_keys::ParticipantKeys,
    replay: Mutex<Option<Replay>>,
    /// Where session records are found: waiting for review in the
    /// recorder, else in this device's record stores.
    recorder: Mutex<Option<Arc<crate::shell::recorder::Recorder>>>,
    records: Mutex<Option<crate::shell::room_records::LocalRecordLookup>>,
    /// Reaches records another member of a room serves.
    custody: Mutex<Option<Arc<crate::shell::record_custody::RecordCustody>>>,
    /// Replays waiting for their record to arrive from a room.
    fetching: Mutex<Vec<(RecordId, UserId, Presentation)>>,
    /// The person at this device, whom out-of-sight checks run for.
    viewer: Mutex<Option<UserId>>,
    /// Offscreen checks by id, and what they ask the frame loop to launch.
    checks: Mutex<std::collections::BTreeMap<u64, offscreen::Check>>,
    check_launches: Mutex<Vec<Launches>>,
    next_check: std::sync::atomic::AtomicU64,
    /// What the last open or reload asks the frame loop to launch.
    launches: Mutex<Launches>,
}

impl std::fmt::Debug for ReplayController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayController")
            .field("replaying", &self.is_replaying())
            .finish_non_exhaustive()
    }
}

impl ReplayController {
    pub fn attach(broker: &BrokerHandle, tree: WorkspaceTreeHandle) -> Arc<Self> {
        Arc::new(Self {
            keys: crate::shell::participant_keys::ParticipantKeys::for_broker(
                broker,
                Arc::clone(&tree),
            ),
            tree,
            fanout: broker.fanout(),
            replay: Mutex::new(None),
            recorder: Mutex::new(None),
            records: Mutex::new(None),
            custody: Mutex::new(None),
            fetching: Mutex::new(Vec::new()),
            viewer: Mutex::new(None),
            checks: Mutex::new(std::collections::BTreeMap::new()),
            check_launches: Mutex::new(Vec::new()),
            next_check: std::sync::atomic::AtomicU64::new(1),
            launches: Mutex::new(Launches::default()),
        })
    }

    pub fn is_replaying(&self) -> bool {
        lock(&self.replay).is_some()
    }

    /// The replay room, while one is open.
    pub fn room(&self) -> Option<RoomId> {
        lock(&self.replay).as_ref().map(|replay| replay.room)
    }

    /// The person at this device, whom out-of-sight checks run for.
    pub fn set_viewer(&self, viewer: UserId) {
        *lock(&self.viewer) = Some(viewer);
    }

    /// Where [`Self::open_record`] looks a record up.
    pub fn set_record_sources(
        &self,
        recorder: Option<Arc<crate::shell::recorder::Recorder>>,
        records: Option<crate::shell::room_records::LocalRecordLookup>,
        custody: Option<Arc<crate::shell::record_custody::RecordCustody>>,
    ) {
        *lock(&self.recorder) = recorder;
        *lock(&self.records) = records;
        *lock(&self.custody) = custody;
    }

    /// The session trace of `record`: a session waiting for review, else a
    /// record in this device's stores.
    ///
    /// # Errors
    ///
    /// When the record is not reachable or not a readable session.
    pub fn resolve(&self, record: &RecordId) -> Result<SessionTrace, TransportError> {
        self.resolve_or_fetch(record)?
            .ok_or_else(|| TransportError::UnknownRecord(record.as_str().to_string()))
    }

    /// The session trace of `record` if this device holds it, else from a
    /// room this device is in whose index references it: `None` while it
    /// is still arriving from the room's other devices.
    fn resolve_or_fetch(&self, record: &RecordId) -> Result<Option<SessionTrace>, TransportError> {
        let local = lock(&self.recorder)
            .as_ref()
            .and_then(|recorder| recorder.session_document(record))
            .or_else(|| {
                lock(&self.records)
                    .as_ref()
                    .and_then(|records| records.load(record))
            });
        let document = if let Some(document) = local {
            document
        } else {
            let custody = lock(&self.custody).clone();
            let Some(custody) = custody else {
                return Err(TransportError::UnknownRecord(record.as_str().to_string()));
            };
            match custody.fetch(record) {
                Ok(Some(document)) => document,
                Ok(None) => return Ok(None),
                Err(_) => {
                    return Err(TransportError::UnknownRecord(record.as_str().to_string()));
                }
            }
        };
        session_trace::read(&document)
            .map(Some)
            .map_err(|error| TransportError::NotASession {
                record: record.as_str().to_string(),
                reason: error.to_string(),
            })
    }

    /// Replay the session record `record` for `owner`. `None` when the
    /// record is still arriving from a room that references it; the replay
    /// opens once it has.
    ///
    /// # Errors
    ///
    /// When the record cannot be found, read or replayed.
    pub fn open_record(
        &self,
        record: &RecordId,
        owner: &UserId,
        presentation: Presentation,
    ) -> Result<Option<RoomId>, TransportError> {
        if let Some(trace) = self.resolve_or_fetch(record)? {
            return self
                .open(trace, Some(record.clone()), owner, presentation)
                .map(Some);
        }
        let mut fetching = lock(&self.fetching);
        fetching.retain(|(waiting, _, _)| waiting != record);
        fetching.push((record.clone(), owner.clone(), presentation));
        drop(fetching);
        Ok(None)
    }

    /// Open the replays whose records arrived.
    fn open_fetched(&self) -> bool {
        let waiting: Vec<(RecordId, UserId, Presentation)> = lock(&self.fetching).clone();
        let mut opened = false;
        for (record, owner, presentation) in waiting {
            match self.resolve_or_fetch(&record) {
                Ok(None) => {}
                Ok(Some(trace)) => {
                    lock(&self.fetching).retain(|(waiting, _, _)| *waiting != record);
                    match self.open(trace, Some(record.clone()), &owner, presentation) {
                        Ok(_) => opened = true,
                        Err(error) => {
                            tracing::warn!(record = %record.as_str(), %error, "replay: the fetched session could not be replayed");
                        }
                    }
                }
                Err(error) => {
                    lock(&self.fetching).retain(|(waiting, _, _)| *waiting != record);
                    tracing::warn!(record = %record.as_str(), %error, "replay: the session could not be reached");
                }
            }
        }
        opened
    }

    /// What the frame loop must launch or clean up, taken once.
    pub fn take_launches(&self) -> Launches {
        std::mem::take(&mut *lock(&self.launches))
    }

    /// Replay `trace` (the content of `record`, when it has one) in a new
    /// replay room owned by `owner`, replacing any replay already open.
    /// Paused at the start; the recorded apps are queued for launch.
    ///
    /// # Errors
    ///
    /// When the trace cannot be replayed.
    pub fn open(
        &self,
        trace: SessionTrace,
        record: Option<RecordId>,
        owner: &UserId,
        presentation: Presentation,
    ) -> Result<RoomId, TransportError> {
        self.close();
        let trace = Arc::new(trace);
        let steps = session_trace::replay::steps(&trace);
        let mut apps: Vec<String> = trace
            .routes()
            .into_iter()
            .map(|route| route.app_id)
            .filter(|app| !app.is_empty())
            .collect();
        apps.dedup();
        let return_to = self.tree.read().desktop.active;
        let (workspace, room, peer) =
            self.open_room(&trace, owner, presentation, &record_name(record.as_ref()))?;
        {
            let mut launches = lock(&self.launches);
            launches.room = Some(room);
            launches.workspace = Some(workspace);
            launches.apps.clone_from(&apps);
            launches.recorded_rooms = recorded_rooms(&trace);
        }
        *lock(&self.replay) = Some(Replay {
            record,
            trace,
            steps,
            playhead: Playhead::new(),
            workspace,
            room,
            owner: owner.clone(),
            peer,
            apps,
            presentation,
            return_to: Some(return_to),
            last_tick: None,
        });
        Ok(room)
    }

    /// Open an ephemeral room, admit the replay peer to it, and join it.
    fn open_room(
        &self,
        trace: &SessionTrace,
        owner: &UserId,
        presentation: Presentation,
        name: &str,
    ) -> Result<(WorkspaceId, RoomId, Arc<ReplayPeer>), TransportError> {
        let (workspace, room) = self.tree.write().open_ephemeral_room(name, owner);
        self.admit(room, trace, owner, presentation)
            .map(|peer| (workspace, room, peer))
    }

    /// Admit the replay peer to `room`, the room of the ephemeral
    /// `workspace`, and join it there. A failure closes the room.
    fn admit(
        &self,
        room: RoomId,
        trace: &SessionTrace,
        owner: &UserId,
        presentation: Presentation,
    ) -> Result<Arc<ReplayPeer>, TransportError> {
        {
            let mut tree = self.tree.write();
            let admitted = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.room == room && candidate.ephemeral)
                .and_then(|candidate| {
                    if candidate
                        .role_of_agent(REPLAY_AGENT)
                        .is_some_and(|role| role != crate::shell::state::CapRole::Denied)
                    {
                        return Some(REPLAY_ADMISSION.to_string());
                    }
                    candidate.grant_as(
                        owner,
                        CapOverride::membership(
                            Subject::agent(REPLAY_AGENT),
                            room.to_string(),
                            Verb::Write,
                        )
                        .with_id(REPLAY_ADMISSION.to_string()),
                    )
                });
            if admitted.is_none() {
                drop(tree);
                self.forget_room(room);
                return Err(TransportError::Room(format!(
                    "{owner} cannot admit the replay peer to its own replay room"
                )));
            }
            // The replayed apps see what was recorded and change nothing:
            // read-only participation, in the room's own vocabulary.
            if let Some(workspace) = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|candidate| candidate.room == room)
            {
                for (app, _) in recorded_rooms(trace) {
                    for capability in [
                        CapOverride::grant(
                            Subject::app(&app),
                            crate::shell::state::Scope::Workspace,
                            Verb::Read,
                        ),
                        CapOverride::new(
                            Subject::app(&app),
                            crate::shell::state::Scope::Workspace,
                            crate::shell::state::Effect::Restrict,
                            Verb::Read,
                        ),
                    ] {
                        let _ = workspace.grant_as(owner, capability);
                    }
                }
            }
            if presentation == Presentation::Shown
                && let Some(workspace) = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|candidate| candidate.room == room)
            {
                tree.desktop.active = workspace.id;
            }
        }
        let sealer = self.keys.sealer(
            crate::shell::participant_keys::Participant::Agent(REPLAY_AGENT.into()),
            &room.to_string(),
        );
        let peer = match ReplayPeer::join(&self.fanout, room.to_string(), trace, sealer) {
            Ok(peer) => peer,
            Err(error) => {
                self.forget_room(room);
                return Err(error.into());
            }
        };
        self.tree.write().admit_collaborator_in_room(
            room,
            Collaborator::new(REPLAY_AGENT, format!("Replay of {}", peer.recorded())).with_role(
                tilo_desktop::workspace_settings::role_label(crate::shell::state::CapRole::Editor),
            ),
        );
        Ok(peer)
    }

    /// Close the replay and tear its room down. Focus returns to where the
    /// person was.
    pub fn close(&self) -> bool {
        let Some(replay) = lock(&self.replay).take() else {
            return false;
        };
        self.keys.forget_room(replay.room);
        let mut tree = self.tree.write();
        let windows = app_windows(&tree, replay.workspace);
        tree.close_ephemeral_room(replay.room);
        if let Some(back) = replay.return_to
            && tree
                .desktop
                .workspaces
                .iter()
                .any(|workspace| workspace.id == back)
            && replay.presentation == Presentation::Shown
        {
            tree.desktop.active = back;
        }
        drop(tree);
        let mut launches = lock(&self.launches);
        launches
            .close_surfaces
            .extend(windows.into_iter().map(|(_, _, surface)| surface));
        launches.closed.push(replay.room);
        true
    }

    /// Close the ephemeral `room` and forget every key it held.
    fn forget_room(&self, room: RoomId) {
        self.keys.forget_room(room);
        self.tree.write().close_ephemeral_room(room);
    }

    fn with<T>(
        &self,
        f: impl FnOnce(&mut Replay) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        let mut guard = lock(&self.replay);
        let replay = guard.as_mut().ok_or(TransportError::NotReplaying)?;
        let result = f(replay);
        drop(guard);
        result
    }

    /// # Errors
    ///
    /// When nothing is being replayed.
    pub fn play(&self) -> Result<(), TransportError> {
        self.with(|replay| {
            if replay.playhead.published() >= replay.trace.timeline.len() {
                // Playing from the end starts over.
                return Ok(true);
            }
            replay.playhead.play();
            replay.last_tick = None;
            Ok(false)
        })
        .and_then(|restart| {
            if restart {
                self.seek_entry(0)?;
                self.with(|replay| {
                    replay.playhead.play();
                    replay.last_tick = None;
                    Ok(())
                })
            } else {
                Ok(())
            }
        })
    }

    /// # Errors
    ///
    /// When nothing is being replayed.
    pub fn pause(&self) -> Result<(), TransportError> {
        self.with(|replay| {
            replay.playhead.pause();
            Ok(())
        })
    }

    /// # Errors
    ///
    /// When nothing is being replayed.
    pub fn set_speed(&self, factor: f64) -> Result<f64, TransportError> {
        self.with(|replay| {
            replay.playhead.set_speed(Speed::from_factor(factor));
            Ok(replay.playhead.speed().factor())
        })
    }

    /// Stop at the end of the next step.
    ///
    /// # Errors
    ///
    /// When nothing is being replayed, or the last step was reached.
    pub fn step_next(&self) -> Result<usize, TransportError> {
        let index = self.with(|replay| {
            replay
                .playhead
                .next_step(&replay.steps)
                .map(|step| step.index)
                .ok_or(TransportError::AtEnd)
        })?;
        self.seek_step(index).map(|()| index)
    }

    /// Stop at the end of the step before the current one, or at the start.
    ///
    /// # Errors
    ///
    /// When nothing is being replayed, or it is already at the start.
    pub fn step_previous(&self) -> Result<Option<usize>, TransportError> {
        let target = self.with(|replay| {
            if replay.playhead.published() == 0 {
                return Err(TransportError::AtStart);
            }
            Ok(replay
                .playhead
                .previous_step(&replay.steps)
                .map(|step| step.index))
        })?;
        match target {
            Some(index) => self.seek_step(index).map(|()| Some(index)),
            None => self.seek_entry(0).map(|()| None),
        }
    }

    /// Stop at the end of step `index`, pausing.
    ///
    /// # Errors
    ///
    /// When nothing is being replayed or there is no such step.
    pub fn seek_step(&self, index: usize) -> Result<(), TransportError> {
        let end = self.with(|replay| {
            replay
                .steps
                .get(index)
                .map(|step| step.timeline.saturating_add(1))
                .ok_or(TransportError::NoSuchStep(index))
        })?;
        self.seek_entry(end)
    }

    /// Stand at `at` in recorded time, paused: every entry recorded at or
    /// before it has happened. The divergence check seeks to each captured
    /// frame's offset this way.
    ///
    /// # Errors
    ///
    /// When nothing is being replayed.
    pub fn seek_offset(&self, at: session_trace::Offset) -> Result<(), TransportError> {
        let end = self.with(|replay| {
            Ok(replay
                .trace
                .timeline
                .iter()
                .take_while(|entry| entry.at <= at)
                .count())
        })?;
        self.seek_entry(end)
    }

    /// The recorded room of each app replayed here, so what is captured in
    /// the replay room can be named as what was recorded.
    pub fn recorded_room_of(&self, app_id: &str) -> Option<String> {
        lock(&self.replay).as_ref().and_then(|replay| {
            replay
                .trace
                .routes()
                .into_iter()
                .find(|route| route.app_id == app_id)
                .map(|route| route.room)
        })
    }

    /// The scale the recorded surfaces were captured at, when the trace
    /// names one.
    pub fn recorded_scale(&self) -> Option<f64> {
        lock(&self.replay).as_ref().and_then(|replay| {
            replay
                .trace
                .environment
                .surfaces
                .iter()
                .map(|surface| surface.scale)
                .find(|scale| scale.is_finite() && *scale > 0.0)
        })
    }

    /// Where the playhead stands in recorded time.
    pub fn at(&self) -> Option<session_trace::Offset> {
        lock(&self.replay)
            .as_ref()
            .map(|replay| replay.playhead.at())
    }

    /// Stand where timeline entries `0..end` have happened, paused. Forward
    /// is a fast-forward; backward reopens the room around the same apps.
    fn seek_entry(&self, end: usize) -> Result<(), TransportError> {
        let (seek, trace, owner, presentation) = self.with(|replay| {
            replay.playhead.pause();
            replay.last_tick = None;
            let seek = replay.playhead.seek_to_entry(&replay.trace, end);
            Ok((
                seek,
                Arc::clone(&replay.trace),
                replay.owner.clone(),
                replay.presentation,
            ))
        })?;
        match seek {
            Seek::Forward(range) => {
                let peer = self.with(|replay| Ok(Arc::clone(&replay.peer)))?;
                peer.publish(range);
                Ok(())
            }
            Seek::Reload { end } => {
                // The same workspace, and so the same windows, in a fresh
                // room: the apps started there ask for the document and get
                // it as it was at the target.
                let (workspace, previous) =
                    self.with(|replay| Ok((replay.workspace, Arc::clone(&replay.peer))))?;
                let (windows, rekeyed) = {
                    let mut tree = self.tree.write();
                    (
                        app_windows(&tree, workspace),
                        tree.rekey_ephemeral_room(workspace),
                    )
                };
                let (closed, room) = rekeyed
                    .ok_or_else(|| TransportError::Room("the replay room is gone".into()))?;
                self.keys.forget_room(closed);
                let peer = self.admit(room, &trace, &owner, presentation)?;
                peer.inherit_checkpoints(&previous);
                drop(previous);
                peer.rewind(end);
                let apps = self.with(|replay| {
                    replay.room = room;
                    replay.peer = peer;
                    Ok(replay.apps.clone())
                })?;
                let mut launches = lock(&self.launches);
                launches.room = Some(room);
                launches.workspace = Some(workspace);
                launches.apps = apps;
                launches.recorded_rooms = recorded_rooms(&trace);
                for (tab, app, surface) in windows {
                    launches.into_tabs.push((app, tab));
                    launches.close_surfaces.push(surface);
                }
                launches.closed.push(closed);
                drop(launches);
                Ok(())
            }
        }
    }

    /// Frame-loop upkeep: publish what became due. Returns whether anything
    /// was published.
    pub fn pump(&self, now: Instant) -> bool {
        let opened = self.open_fetched();
        let (peer, range) = {
            let mut guard = lock(&self.replay);
            let Some(replay) = guard.as_mut() else {
                return opened;
            };
            let elapsed = replay
                .last_tick
                .map_or_else(Default::default, |last| now.saturating_duration_since(last));
            replay.last_tick = replay.playhead.is_playing().then_some(now);
            let range = replay.playhead.tick(&replay.trace, elapsed);
            let peer = Arc::clone(&replay.peer);
            drop(guard);
            (peer, range)
        };
        if range.is_empty() {
            return opened;
        }
        peer.publish(range);
        true
    }

    /// The peer of the open replay, for tests and inspection.
    pub fn peer(&self) -> Option<Arc<ReplayPeer>> {
        lock(&self.replay)
            .as_ref()
            .map(|replay| Arc::clone(&replay.peer))
    }

    /// What `state-dump` reports under `replay`.
    pub fn status_json(&self) -> serde_json::Value {
        let guard = lock(&self.replay);
        let Some(replay) = guard.as_ref() else {
            return serde_json::Value::Null;
        };
        let current = replay
            .playhead
            .current_step(&replay.steps)
            .map(|step| step.index);
        let status = serde_json::json!({
            "record": replay.record.as_ref().map(RecordId::as_str),
            "room": replay.room.to_string(),
            "workspace": replay.workspace.raw(),
            "presentation": replay.presentation,
            "apps": replay.apps,
            "followed": replay.peer.followed(),
            "recorded": replay.peer.recorded(),
            "playing": replay.playhead.is_playing(),
            "speed": replay.playhead.speed().factor(),
            "at_ms": replay.playhead.at().as_millis(),
            "duration_ms": replay.trace.duration.as_millis(),
            "published": replay.playhead.published(),
            "events": replay.trace.timeline.len(),
            "step": current,
            "steps": replay.steps.iter().map(step_json).collect::<Vec<_>>(),
            "protected": protected_apps(&replay.trace),
        });
        drop(guard);
        status
    }
}

impl Drop for ReplayController {
    fn drop(&mut self) {
        self.close();
    }
}

fn record_name(record: Option<&RecordId>) -> String {
    record.map_or_else(
        || "Replay".to_string(),
        |record| format!("Replay {}", record.as_str()),
    )
}

fn step_json(step: &Step) -> serde_json::Value {
    let (kind, participant, outcome) = match &step.kind {
        StepKind::Interaction {
            participant, phase, ..
        } => (
            "interaction",
            participant.as_str(),
            Some(match phase {
                tilo_actions::InteractionPhase::Committed => "committed",
                tilo_actions::InteractionPhase::Failed { .. } => "failed",
                tilo_actions::InteractionPhase::Cancelled { .. } => "cancelled",
                tilo_actions::InteractionPhase::Rejected { .. } => "rejected",
                _ => "open",
            }),
        ),
        StepKind::View { participant } => ("view", participant.as_str(), None),
    };
    serde_json::json!({
        "index": step.index,
        "at_ms": step.at.as_millis(),
        "kind": kind,
        "participant": participant,
        "outcome": outcome,
    })
}
