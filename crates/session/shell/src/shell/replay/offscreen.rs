//! A replay nobody watches: the check a recording is put through before it
//! leaves the device (`docs/architecture/feedback.md`, "Two layers").
//!
//! The session is replayed in a hidden ephemeral room (`Workspace::hidden`):
//! a room like any other that never becomes active and is left out of every
//! place that lists workspaces to a person. At each offset the recording
//! captured frames at, the replay peer publishes the timeline up to it, the
//! replayed apps are given time to draw, and the compositor renders their
//! surfaces through the same path a recording captures through, at the
//! recorded size and scale. What is collected is a
//! [`FrameProvider`](session_trace::frames::FrameProvider) the review hands
//! to `session_trace::divergence::check`.
//!
//! The replayed apps render in their own processes, so this is driven by the
//! compositor's frame loop ([`ReplayController::offscreen_captures`] and
//! [`ReplayController::deliver_captures`]) rather than inside the check.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use session_trace::frames::{FrameError, FrameProvider, FrameStrip, SurfaceFrame};
use session_trace::{Offset, Route, SessionTrace};
use ui::record::RecordId;
use vello::kurbo::Rect;

use super::{Launches, Presentation, ReplayController, ReplayPeer, app_windows, lock};
use crate::shell::state::{RoomId, WorkspaceId};

/// How long a check may run before it is given up.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a replayed app gets to reach a seek's recorded frame before
/// its surface is captured as it is.
const SETTLE: Duration = Duration::from_millis(1500);
/// A seek always waits at least this long, so an app's commit for it can
/// arrive.
const MIN_SETTLE: Duration = Duration::from_millis(120);
/// The least time between two captures of one seek.
const RECAPTURE_GAP: Duration = Duration::from_millis(50);

/// A check's room, its app windows (surface, app) and the shots its first
/// target is to be captured at.
type CheckWindows = (WorkspaceId, Vec<(u64, String)>, Vec<Shot>);

/// One surface to capture at one offset: its recorded route, and the size
/// and scale it was recorded at.
#[derive(Clone, Debug)]
struct Shot {
    route: Route,
    rect: Rect,
    scale: f64,
}

struct Target {
    at: Offset,
    shots: Vec<Shot>,
}

/// A seek waiting for the replayed apps to show it.
///
/// What a seek publishes reaches an app along separate paths: presence at
/// once, a document change once the app has opened it with the channel's
/// keys and merged it. A frame drawn after the seek may show the first and
/// not yet the second. So each frame drawn after the seek is captured and
/// compared with the recorded one, and the seek settles on the first that
/// matches, or on what the app shows once [`SETTLE`] has passed: a replay
/// diverges only when it never reaches what was recorded.
struct Settling {
    since: Instant,
    /// Each replayed surface's frame sequence when the seek was made, or
    /// when it was last captured.
    seqs: Vec<(u64, u64)>,
    captured: Option<Instant>,
}

impl Settling {
    fn new(now: Instant, surfaces: impl Iterator<Item = u64>) -> Self {
        Self {
            since: now,
            seqs: surfaces
                .map(|surface| (surface, crate::shell::foreign::store().frame_seq(surface)))
                .collect(),
            captured: None,
        }
    }

    /// Whether to capture at `now`: every surface drew since the seek or
    /// the last capture, or [`SETTLE`] passed.
    fn due(&self, now: Instant) -> bool {
        let waited = now.saturating_duration_since(self.since);
        if waited >= SETTLE {
            return true;
        }
        let gap = self
            .captured
            .is_none_or(|at| now.saturating_duration_since(at) >= RECAPTURE_GAP);
        waited >= MIN_SETTLE
            && gap
            && self
                .seqs
                .iter()
                .all(|(surface, seq)| crate::shell::foreign::store().frame_seq(*surface) > *seq)
    }

    fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= SETTLE
    }

    /// A capture at `now` did not show the recorded frame: wait for the
    /// next one.
    fn missed(&mut self, now: Instant) {
        self.captured = Some(now);
        for (surface, seq) in &mut self.seqs {
            *seq = crate::shell::foreign::store().frame_seq(*surface);
        }
    }
}

pub(super) struct Check {
    trace: Arc<SessionTrace>,
    workspace: WorkspaceId,
    room: RoomId,
    peer: Arc<ReplayPeer>,
    targets: Vec<Target>,
    /// The recorded frames each capture is compared with, and the layout
    /// they are compared in.
    strip: FrameStrip,
    layout: session_trace::frames::SemanticLayout,
    next: usize,
    settling: Option<Settling>,
    frames: Vec<SurfaceFrame>,
    done: Option<Result<Vec<SurfaceFrame>, String>>,
    started: Instant,
    /// Why the last capture of a surface failed, if one did.
    last_error: Option<String>,
}

/// One capture the frame loop makes for a check.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub check: u64,
    pub at: Offset,
    pub scale: f64,
    /// `(surface id, recorded route, logical rect)`, as
    /// `CaptureExecutor::surface_frames` takes them.
    pub surfaces: Vec<(u64, Route, Rect)>,
}

/// The frames an offscreen replay produced, by offset.
#[derive(Clone, Debug, Default)]
pub struct CollectedFrames {
    frames: Vec<SurfaceFrame>,
}

impl FrameProvider for CollectedFrames {
    fn frames_at(&mut self, at: Offset) -> Result<Vec<SurfaceFrame>, FrameError> {
        Ok(self
            .frames
            .iter()
            .filter(|frame| frame.at == at)
            .cloned()
            .collect())
    }
}

/// What to capture at each offset of `strip`.
fn targets(strip: &FrameStrip) -> Vec<Target> {
    let mut by_offset: BTreeMap<Offset, Vec<Shot>> = BTreeMap::new();
    for frame in &strip.frames {
        let scale = if frame.scale.is_finite() && frame.scale > 0.0 {
            frame.scale
        } else {
            1.0
        };
        let shots = by_offset.entry(frame.at).or_default();
        if shots
            .iter()
            .any(|shot| session_trace::frames::same_surface(&shot.route, &frame.route))
        {
            continue;
        }
        shots.push(Shot {
            route: frame.route.clone(),
            rect: Rect::new(
                0.0,
                0.0,
                f64::from(frame.width) / scale,
                f64::from(frame.height) / scale,
            ),
            scale,
        });
    }
    by_offset
        .into_iter()
        .map(|(at, shots)| Target { at, shots })
        .collect()
}

impl ReplayController {
    /// Replay the session record `record` out of sight and capture its
    /// surfaces at every offset `strip` captured the recording at, at the
    /// recorded size and scale. Returns the check's id; the frames come from
    /// [`Self::take_check`].
    ///
    /// # Errors
    ///
    /// When the record cannot be found, read or replayed.
    pub fn start_check(&self, record: &RecordId, strip: &FrameStrip) -> Result<u64, String> {
        let trace = self.resolve(record).map_err(|error| error.to_string())?;
        self.start_check_of(trace, &format!("Check {}", record.as_str()), strip)
    }

    /// [`Self::start_check`] for a trace in hand, in a room named `name`.
    ///
    /// # Errors
    ///
    /// When the trace cannot be replayed or `strip` holds no frames.
    pub fn start_check_of(
        &self,
        trace: SessionTrace,
        name: &str,
        strip: &FrameStrip,
    ) -> Result<u64, String> {
        let targets = targets(strip);
        if targets.is_empty() {
            return Err("the recording kept no frames to check against".into());
        }
        let owner = lock(&self.viewer)
            .clone()
            .unwrap_or_else(|| super::REPLAY_AGENT.to_string());
        let (workspace, room) = self
            .tree
            .write()
            .open_ephemeral_room_shown(name, &owner, false);
        let peer = self
            .admit(room, &trace, &owner, Presentation::Offscreen)
            .map_err(|error| error.to_string())?;
        let mut apps: Vec<String> = trace
            .routes()
            .into_iter()
            .map(|route| route.app_id)
            .filter(|app| !app.is_empty())
            .collect();
        apps.dedup();
        let id = self
            .next_check
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        lock(&self.check_launches).push(Launches {
            room: Some(room),
            workspace: Some(workspace),
            apps,
            recorded_rooms: super::recorded_rooms(&trace),
            ..Launches::default()
        });
        lock(&self.checks).insert(
            id,
            Check {
                layout: session_trace::frames::SemanticLayout::from_trace(&trace),
                trace: Arc::new(trace),
                workspace,
                room,
                peer,
                targets,
                strip: strip.clone(),
                next: 0,
                settling: None,
                frames: Vec::new(),
                done: None,
                last_error: None,
                started: Instant::now(),
            },
        );
        Ok(id)
    }

    /// The frames of check `id` once every offset was captured, taking
    /// them; `None` while it runs.
    pub fn take_check(&self, id: u64) -> Option<Result<CollectedFrames, String>> {
        let mut checks = lock(&self.checks);
        let done = checks.get_mut(&id)?.done.take()?;
        checks.remove(&id);
        drop(checks);
        Some(done.map(|frames| CollectedFrames { frames }))
    }

    /// What `state-dump` reports of review under `checks`: each check out of
    /// sight, the offsets it captures and how many it has, and the windows
    /// its room holds.
    pub fn checks_json(&self) -> serde_json::Value {
        let checks = lock(&self.checks);
        let tree = self.tree.read();
        let out: Vec<serde_json::Value> = checks
            .iter()
            .map(|(id, check)| {
                serde_json::json!({
                    "check": id,
                    "room": check.room.to_string(),
                    "offsets_ms": check.targets.iter().map(|target| target.at.as_millis()).collect::<Vec<_>>(),
                    "next": check.next,
                    "settling": check.settling.is_some(),
                    "frames": check.frames.len(),
                    "done": check.done.as_ref().map(|done| match done {
                        Ok(frames) => serde_json::json!({ "frames": frames.len() }),
                        Err(reason) => serde_json::json!({ "failed": reason }),
                    }),
                    "windows": app_windows(&tree, check.workspace)
                        .into_iter()
                        .map(|(_, app, surface)| serde_json::json!({ "app": app, "surface": surface }))
                        .collect::<Vec<_>>(),
                    "running_ms": u64::try_from(check.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    "last_error": check.last_error,
                })
            })
            .collect();
        drop(tree);
        drop(checks);
        serde_json::Value::Array(out)
    }

    /// Whether a check out of sight is still running.
    pub fn checking(&self) -> bool {
        lock(&self.checks)
            .values()
            .any(|check| check.done.is_none())
    }

    /// Launches the checks asked for, taken once.
    pub fn take_check_launches(&self) -> Vec<Launches> {
        std::mem::take(&mut *lock(&self.check_launches))
    }

    /// Whether `surface` is a replayed app of an offscreen check, and the
    /// size its window is to be at, in logical pixels.
    pub fn offscreen_surface_size(&self, surface: u64) -> Option<(f64, f64)> {
        let windows: Vec<CheckWindows> = {
            let checks = lock(&self.checks);
            let tree = self.tree.read();
            checks
                .values()
                .map(|check| {
                    (
                        check.workspace,
                        app_windows(&tree, check.workspace)
                            .into_iter()
                            .map(|(_, app, id)| (id, app))
                            .collect(),
                        check
                            .targets
                            .first()
                            .map(|target| target.shots.clone())
                            .unwrap_or_default(),
                    )
                })
                .collect()
        };
        windows.into_iter().find_map(|(_, windows, shots)| {
            let (_, app) = windows.iter().find(|(id, _)| *id == surface)?;
            let shot = shots
                .iter()
                .find(|shot| {
                    crate::shell::launcher::app_id_matches_package(app, &shot.route.app_id)
                })
                .or_else(|| shots.first())?;
            Some((shot.rect.width(), shot.rect.height()))
        })
    }

    /// Frame-loop upkeep for the offscreen checks: seek each to its next
    /// offset, and once its apps have had time to draw there, ask for their
    /// surfaces to be captured.
    pub fn offscreen_captures(&self, now: Instant) -> Vec<CaptureRequest> {
        let mut requests = Vec::new();
        let mut finished: Vec<(RoomId, Vec<u64>)> = Vec::new();
        // Published once the tree is released: sealing asks it who is
        // admitted.
        let mut due: Vec<(Arc<ReplayPeer>, std::ops::Range<usize>)> = Vec::new();
        let tree = self.tree.read();
        let mut checks = lock(&self.checks);
        for (id, check) in checks.iter_mut() {
            if check.done.is_some() {
                continue;
            }
            if now.saturating_duration_since(check.started) > CHECK_TIMEOUT {
                check.done = Some(Err("the replay did not finish in time".into()));
                finished.push((
                    check.room,
                    app_windows(&tree, check.workspace)
                        .into_iter()
                        .map(|(_, _, surface)| surface)
                        .collect(),
                ));
                continue;
            }
            let windows = app_windows(&tree, check.workspace);
            // Every recorded app has a window here that has drawn: a window
            // still waiting for its first frame has nothing to compare.
            let apps_up = check.targets.get(check.next).is_some_and(|target| {
                target.shots.iter().all(|shot| {
                    windows.iter().any(|(_, app, surface)| {
                        crate::shell::launcher::app_id_matches_package(app, &shot.route.app_id)
                            && crate::shell::foreign::store().frame_seq(*surface) > 0
                    })
                })
            });
            if !apps_up {
                continue;
            }
            let Some(target) = check.targets.get(check.next) else {
                continue;
            };
            let Some(settling) = check.settling.as_mut() else {
                // Seek: every entry recorded at or before the offset has
                // happened, published to the apps.
                let end = check
                    .trace
                    .timeline
                    .iter()
                    .take_while(|entry| entry.at <= target.at)
                    .count();
                due.push((Arc::clone(&check.peer), 0..end));
                check.settling = Some(Settling::new(
                    now,
                    windows.iter().map(|(_, _, surface)| *surface),
                ));
                continue;
            };
            if !settling.due(now) {
                continue;
            }
            let surfaces = target
                .shots
                .iter()
                .filter_map(|shot| {
                    let (_, _, surface) = windows.iter().find(|(_, app, _)| {
                        crate::shell::launcher::app_id_matches_package(app, &shot.route.app_id)
                    })?;
                    Some((*surface, shot.route.clone(), shot.rect))
                })
                .collect();
            requests.push(CaptureRequest {
                check: *id,
                at: target.at,
                scale: target.shots.first().map_or(1.0, |shot| shot.scale),
                surfaces,
            });
        }
        drop(checks);
        drop(tree);
        for (peer, range) in due {
            peer.publish(range);
        }
        for (room, surfaces) in finished {
            self.close_check_room(room, surfaces);
        }
        requests
    }

    /// What the frame loop captured for `request`: kept, and the check moves
    /// to its next offset, or finishes.
    pub fn deliver_captures(
        &self,
        request: &CaptureRequest,
        frames: Vec<Result<SurfaceFrame, String>>,
    ) {
        let finished = {
            let mut checks = lock(&self.checks);
            let Some(check) = checks.get_mut(&request.check) else {
                return;
            };
            let now = Instant::now();
            if !check
                .settling
                .as_ref()
                .is_none_or(|settling| settling.expired(now))
                && !shows_recorded(check, request.at, &frames)
            {
                if let Some(settling) = check.settling.as_mut() {
                    settling.missed(now);
                }
                return;
            }
            for frame in frames {
                match frame {
                    Ok(frame) => check.frames.push(frame),
                    Err(error) => {
                        tracing::debug!(%error, "replay check: a surface could not be captured");
                        check.last_error = Some(error);
                    }
                }
            }
            check.settling = None;
            check.next = check.next.saturating_add(1);
            let finished = if check.next < check.targets.len() {
                None
            } else {
                check.done = Some(Ok(std::mem::take(&mut check.frames)));
                Some((check.room, check.workspace))
            };
            drop(checks);
            finished
        };
        if let Some((room, workspace)) = finished {
            let surfaces = app_windows(&self.tree.read(), workspace)
                .into_iter()
                .map(|(_, _, surface)| surface)
                .collect();
            self.close_check_room(room, surfaces);
        }
    }

    fn close_check_room(&self, room: RoomId, surfaces: Vec<u64>) {
        self.forget_room(room);
        lock(&self.check_launches).push(Launches {
            close_surfaces: surfaces,
            closed: vec![room],
            ..Launches::default()
        });
    }
}

/// Whether `frames`, captured for offset `at`, show what the recording's
/// frames there showed, compared as review compares them.
fn shows_recorded(check: &Check, at: Offset, frames: &[Result<SurfaceFrame, String>]) -> bool {
    let tolerance = session_trace::divergence::Tolerance::default();
    frames.iter().all(|frame| {
        let Ok(frame) = frame else {
            return false;
        };
        check
            .strip
            .frames
            .iter()
            .filter(|stored| stored.at == at && frame.same_surface(&stored.route))
            .all(|stored| {
                let regions = check.layout.for_comparison(&stored.route, at);
                session_trace::divergence::compare_frame(stored, Some(frame), &regions, &tolerance)
                    .is_ok_and(|step| step.regions.is_empty())
            })
    })
}

impl crate::shell::remark::OffscreenReplay for ReplayController {
    fn start(&self, record: &RecordId, frames: &FrameStrip) -> Result<u64, String> {
        self.start_check(record, frames)
    }

    fn take(&self, check: u64) -> Option<Result<Box<dyn FrameProvider + Send>, String>> {
        self.take_check(check)
            .map(|done| done.map(|frames| Box::new(frames) as Box<dyn FrameProvider + Send>))
    }
}
