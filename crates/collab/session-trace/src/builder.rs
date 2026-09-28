//! Assembling a trace as events arrive.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use collab_types::{DocumentFrame, DocumentMutation, DocumentMutationKind, SignedOp};

use crate::{
    DocumentOp, Environment, Event, InspectKeyframe, Offset, Origin, PresenceUpdate, Redactions,
    Route, RouteSnapshot, SessionLayer, SessionTrace, SnapshotPoint, TimedEvent,
};

/// Bounds on one trace. Hitting any of them marks the trace truncated and
/// drops what follows; nothing already kept is discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_events: usize,
    /// Approximate encoded bytes across snapshots and timeline.
    pub max_bytes: usize,
    pub max_routes: usize,
    pub max_duration: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_events: 50_000,
            max_bytes: 64 * 1024 * 1024,
            max_routes: 64,
            max_duration: Duration::from_secs(30 * 60),
        }
    }
}

/// What a push did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// Appended to the timeline.
    Timeline,
    /// Taken as (part of) the route's snapshot.
    Snapshot,
    /// A bound was hit; the event was dropped and the trace is truncated.
    Full,
}

/// Segments of a snapshot transfer still arriving, keyed by transfer digest.
struct PendingSnapshot {
    from: String,
    at: Offset,
    expected: u32,
    frames: Vec<(u32, SignedOp)>,
}

/// Builds a [`SessionTrace`] from events in delivery order.
pub struct TraceBuilder {
    origin: Origin,
    started_unix_ms: u64,
    epoch: Instant,
    last: Offset,
    limits: Limits,
    bytes: usize,
    environment: Environment,
    snapshot_point: SnapshotPoint,
    snapshots: Vec<RouteSnapshot>,
    pending: BTreeMap<(Route, [u8; 32]), PendingSnapshot>,
    routes: Vec<Route>,
    timeline: Vec<TimedEvent>,
    tags: Vec<String>,
    truncated: bool,
}

impl TraceBuilder {
    /// A trace whose offsets count from `epoch`, which happened at wall
    /// clock `started`.
    pub fn new(origin: Origin, epoch: Instant, started: SystemTime, limits: Limits) -> Self {
        let started_unix_ms = started
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            });
        Self {
            origin,
            started_unix_ms,
            epoch,
            last: Offset::default(),
            limits,
            bytes: 0,
            environment: Environment::default(),
            snapshot_point: match origin {
                Origin::Explicit => SnapshotPoint::Start,
                Origin::Retroactive => SnapshotPoint::End,
            },
            snapshots: Vec::new(),
            pending: BTreeMap::new(),
            routes: Vec::new(),
            timeline: Vec::new(),
            tags: Vec::new(),
            truncated: false,
        }
    }

    /// Replace the environment, keeping the protection markers already
    /// noted.
    pub fn set_environment(&mut self, environment: Environment) {
        let protected = std::mem::take(&mut self.environment.protected);
        self.environment = environment;
        for marker in protected {
            self.note_protected(&marker.app_id, &marker.room);
        }
    }

    /// Record the geometry of an app surface, replacing what was noted for
    /// that app before. Bounded like the routes.
    pub fn note_surface(&mut self, surface: crate::SurfaceGeometry) {
        let surfaces = &mut self.environment.surfaces;
        if let Some(existing) = surfaces
            .iter_mut()
            .find(|known| known.app_id == surface.app_id)
        {
            *existing = surface;
        } else if surfaces.len() < self.limits.max_routes {
            surfaces.push(surface);
        }
    }

    /// Mark `app_id` as present in `room` and capture-protected: the one
    /// thing a session keeps of it. Bounded like the routes.
    pub fn note_protected(&mut self, app_id: &str, room: &str) {
        let marker = crate::ProtectedApp {
            app_id: app_id.to_string(),
            room: room.to_string(),
        };
        let protected = &mut self.environment.protected;
        if let Err(at) = protected.binary_search(&marker)
            && protected.len() < self.limits.max_routes
        {
            protected.insert(at, marker);
        }
    }

    pub fn add_tag(&mut self, tag: impl Into<String>) {
        let tag = tag.into();
        if !tag.is_empty() && tag.len() <= 64 && self.tags.len() < 32 && !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn event_count(&self) -> usize {
        self.timeline.len()
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Semantic-tree keyframes on the timeline so far.
    pub fn keyframe_count(&self) -> usize {
        self.timeline
            .iter()
            .filter(|entry| matches!(entry.event, Event::Inspect(_)))
            .count()
    }

    /// Whether `route` already has its snapshot.
    pub fn has_snapshot(&self, route: &Route) -> bool {
        self.snapshots
            .iter()
            .any(|snapshot| &snapshot.route == route)
    }

    /// Where `at` falls on this trace's clock, without placing anything
    /// on the timeline. The pixel layer stamps frames with it.
    pub fn offset_at(&self, at: Instant) -> Offset {
        Offset::from_duration(at.saturating_duration_since(self.epoch))
    }

    /// The semantic layout of `surface` as the timeline last described it.
    pub fn latest_regions(&self, surface: &Route) -> Vec<crate::frames::ElementRegion> {
        self.timeline
            .iter()
            .rev()
            .find_map(|entry| match &entry.event {
                Event::Inspect(keyframe)
                    if crate::frames::same_surface(&keyframe.route, surface) =>
                {
                    Some(crate::frames::keyframe_regions(keyframe))
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Monotonic offset of `at`: never before the previous event, so the
    /// timeline stays ordered even if callers' clocks disagree.
    fn offset(&mut self, at: Instant) -> Offset {
        let offset = Offset::from_duration(at.saturating_duration_since(self.epoch)).max(self.last);
        self.last = offset;
        offset
    }

    fn admit(&mut self, at: Instant, weight: usize, route: Option<&Route>) -> Option<Offset> {
        if self.truncated {
            return None;
        }
        if let Some(route) = route
            && !self.routes.contains(route)
        {
            if self.routes.len() >= self.limits.max_routes {
                self.truncated = true;
                return None;
            }
            self.routes.push(route.clone());
        }
        let offset = self.offset(at);
        let bytes = self.bytes.saturating_add(weight);
        if offset.as_duration() > self.limits.max_duration
            || self.timeline.len() >= self.limits.max_events
            || bytes > self.limits.max_bytes
        {
            self.truncated = true;
            return None;
        }
        self.bytes = bytes;
        Some(offset)
    }

    /// A signed document op delivered on `route`. The first snapshot a route
    /// receives becomes its [`RouteSnapshot`] (all segments of a segmented
    /// transfer, once complete); everything else joins the timeline.
    pub fn push_op(
        &mut self,
        at: Instant,
        route: Route,
        from: String,
        envelope: SignedOp,
    ) -> Pushed {
        if !self.has_snapshot(&route) && envelope.encryption.is_none() {
            match DocumentFrame::decode(&envelope.payload) {
                Ok(DocumentFrame::Whole(DocumentMutation::Snapshot(_))) => {
                    return self.take_snapshot(at, route, from, vec![envelope]);
                }
                Ok(DocumentFrame::Segment(segment))
                    if segment.kind == DocumentMutationKind::Snapshot =>
                {
                    return self.take_segment(
                        at,
                        route,
                        from,
                        segment.digest,
                        segment.index,
                        segment.count(),
                        envelope,
                    );
                }
                _ => {}
            }
        }
        self.push_timeline_op(at, route, from, envelope)
    }

    /// A signed document op that belongs on the timeline whatever it
    /// carries — an op served from history into a retroactive trace.
    pub fn push_timeline_op(
        &mut self,
        at: Instant,
        route: Route,
        from: String,
        envelope: SignedOp,
    ) -> Pushed {
        let key = route.clone();
        let event = Event::Op(DocumentOp {
            route,
            from,
            envelope,
        });
        self.push_event(at, Some(&key), event)
    }

    fn take_segment(
        &mut self,
        at: Instant,
        route: Route,
        from: String,
        digest: [u8; 32],
        index: u32,
        count: u32,
        envelope: SignedOp,
    ) -> Pushed {
        let weight = envelope.payload.len().saturating_add(192);
        let Some(offset) = self.admit(at, weight, Some(&route)) else {
            return Pushed::Full;
        };
        let key = (route.clone(), digest);
        let pending = self
            .pending
            .entry(key.clone())
            .or_insert_with(|| PendingSnapshot {
                from,
                at: offset,
                expected: count,
                frames: Vec::new(),
            });
        if !pending.frames.iter().any(|(seen, _)| *seen == index) {
            pending.frames.push((index, envelope));
        }
        let complete = u32::try_from(pending.frames.len()).unwrap_or(u32::MAX) >= pending.expected;
        if complete && let Some(mut done) = self.pending.remove(&key) {
            done.frames.sort_by_key(|(index, _)| *index);
            // Other in-flight transfers for this route lose: one snapshot per route.
            self.pending
                .retain(|(pending_route, _), _| pending_route != &route);
            self.snapshots.push(RouteSnapshot {
                route,
                from: done.from,
                at: done.at,
                point: self.snapshot_point,
                frames: done.frames.into_iter().map(|(_, frame)| frame).collect(),
            });
        }
        Pushed::Snapshot
    }

    fn take_snapshot(
        &mut self,
        at: Instant,
        route: Route,
        from: String,
        frames: Vec<SignedOp>,
    ) -> Pushed {
        let weight = frames.iter().fold(192_usize, |sum, frame| {
            sum.saturating_add(frame.payload.len())
        });
        let Some(offset) = self.admit(at, weight, Some(&route)) else {
            return Pushed::Full;
        };
        self.pending
            .retain(|(pending_route, _), _| pending_route != &route);
        self.snapshots.push(RouteSnapshot {
            route,
            from,
            at: offset,
            point: self.snapshot_point,
            frames,
        });
        Pushed::Snapshot
    }

    pub fn push_presence(&mut self, at: Instant, update: PresenceUpdate) -> Pushed {
        self.push_event(at, None, Event::Presence(update))
    }

    pub fn push_inspect(&mut self, at: Instant, keyframe: InspectKeyframe) -> Pushed {
        let route = keyframe.route.clone();
        self.push_event(at, Some(&route), Event::Inspect(keyframe))
    }

    fn push_event(&mut self, at: Instant, route: Option<&Route>, event: Event) -> Pushed {
        let Some(offset) = self.admit(at, event.weight(), route) else {
            return Pushed::Full;
        };
        self.timeline.push(TimedEvent { at: offset, event });
        Pushed::Timeline
    }

    /// Close the trace at `at`. Snapshot transfers still missing segments are
    /// dropped: a partial snapshot cannot be loaded.
    pub fn finish(mut self, at: Instant) -> SessionTrace {
        let duration = self
            .offset(at)
            .min(Offset::from_duration(self.limits.max_duration));
        SessionTrace {
            origin: self.origin,
            started_unix_ms: self.started_unix_ms,
            duration,
            environment: self.environment,
            snapshots: self.snapshots,
            timeline: self.timeline,
            tags: self.tags,
            layer: SessionLayer::Logic,
            redactions: Redactions::default(),
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{incremental, interaction, presence, route, snapshot, snapshot_segments};
    use crate::{Event, PresenceChange};

    fn builder(limits: Limits) -> (TraceBuilder, Instant) {
        let epoch = Instant::now();
        (
            TraceBuilder::new(Origin::Explicit, epoch, SystemTime::now(), limits),
            epoch,
        )
    }

    #[test]
    fn first_snapshot_per_route_is_the_start_and_everything_else_is_ordered_timeline() {
        let (mut trace, epoch) = builder(Limits::default());
        let notes = route("so.tilo.notes");
        let at = |ms| epoch + Duration::from_millis(ms);
        assert_eq!(
            trace.push_op(at(5), notes.clone(), "alice".into(), incremental(1)),
            Pushed::Timeline,
            "an op before the snapshot is kept, replay deduplicates it"
        );
        assert_eq!(
            trace.push_op(at(10), notes.clone(), "alice".into(), snapshot(2)),
            Pushed::Snapshot
        );
        assert_eq!(
            trace.push_presence(at(20), presence("alice", PresenceChange::Caret { pos: 4 })),
            Pushed::Timeline
        );
        assert_eq!(
            trace.push_op(at(30), notes.clone(), "alice".into(), snapshot(3)),
            Pushed::Timeline,
            "a later snapshot is an ordinary delivery"
        );
        // A caller whose clock runs behind still lands after what came before.
        assert_eq!(
            trace.push_op(at(1), notes, "alice".into(), incremental(4)),
            Pushed::Timeline
        );
        let trace = trace.finish(at(50));
        assert_eq!(trace.snapshots.len(), 1);
        assert_eq!(trace.snapshots[0].frames[0].seq, 2);
        assert_eq!(trace.snapshots[0].point, SnapshotPoint::Start);
        let offsets: Vec<u64> = trace.events().map(|entry| entry.at.0).collect();
        assert!(
            offsets.windows(2).all(|pair| pair[0] <= pair[1]),
            "{offsets:?}"
        );
        assert_eq!(offsets, vec![5_000, 20_000, 30_000, 30_000]);
        assert_eq!(trace.duration.as_millis(), 50);
        assert!(matches!(&trace.timeline[3].event, Event::Op(op) if op.envelope.seq == 4));
    }

    #[test]
    fn segmented_snapshot_is_taken_once_every_segment_arrived() {
        let (mut trace, epoch) = builder(Limits::default());
        let notes = route("so.tilo.notes");
        let segments = snapshot_segments(3);
        assert_eq!(segments.len(), 3);
        for (index, segment) in segments.iter().enumerate().rev() {
            assert_eq!(
                trace.push_op(epoch, notes.clone(), "bob".into(), segment.clone()),
                Pushed::Snapshot
            );
            assert_eq!(trace.has_snapshot(&notes), index == 0);
        }
        let trace = trace.finish(epoch);
        let frames = &trace.snapshot_of(&notes).expect("snapshot").frames;
        assert_eq!(frames, &segments, "segments are kept in transfer order");
        assert!(trace.timeline.is_empty());
    }

    #[test]
    fn bounds_truncate_instead_of_growing() {
        let (mut trace, epoch) = builder(Limits {
            max_events: 2,
            ..Limits::default()
        });
        for n in 0..4 {
            let pushed = trace.push_op(epoch, route("so.tilo.notes"), "a".into(), incremental(n));
            assert_eq!(
                pushed,
                if n < 2 {
                    Pushed::Timeline
                } else {
                    Pushed::Full
                }
            );
        }
        let trace = trace.finish(epoch);
        assert!(trace.truncated);
        assert_eq!(trace.timeline.len(), 2);

        let (mut by_bytes, epoch) = builder(Limits {
            max_bytes: 1_000,
            ..Limits::default()
        });
        let mut big = incremental(1);
        big.payload = vec![0; 2_000];
        assert_eq!(
            by_bytes.push_op(epoch, route("a"), "a".into(), big),
            Pushed::Full
        );
        assert!(by_bytes.is_truncated());

        let (mut surfaces, epoch_s) = builder(Limits {
            max_routes: 1,
            ..Limits::default()
        });
        let surface = |app: &str, width| crate::SurfaceGeometry {
            app_id: app.into(),
            width,
            height: 600,
            scale: 2.0,
            output: "DP-1".into(),
        };
        surfaces.note_surface(surface("so.tilo.notes", 800));
        surfaces.note_surface(surface("so.tilo.notes", 1024));
        surfaces.note_surface(surface("so.tilo.mail", 640));
        assert_eq!(
            surfaces.finish(epoch_s).environment.surfaces,
            vec![surface("so.tilo.notes", 1024)],
            "a surface's latest geometry replaces the earlier one, within bounds"
        );

        let (mut by_routes, epoch) = builder(Limits {
            max_routes: 1,
            ..Limits::default()
        });
        assert_eq!(
            by_routes.push_op(epoch, route("a"), "a".into(), incremental(1)),
            Pushed::Timeline
        );
        assert_eq!(
            by_routes.push_op(epoch, route("b"), "a".into(), incremental(2)),
            Pushed::Full
        );

        let (mut by_time, epoch) = builder(Limits {
            max_duration: Duration::from_secs(1),
            ..Limits::default()
        });
        assert_eq!(
            by_time.push_presence(
                epoch + Duration::from_secs(2),
                presence("a", PresenceChange::Arrived)
            ),
            Pushed::Full
        );
        assert_eq!(
            by_time
                .finish(epoch + Duration::from_secs(5))
                .duration
                .as_millis(),
            1_000
        );
    }

    #[test]
    fn summary_counts_steps_by_invocation_and_remembers_failures() {
        let (mut trace, epoch) = builder(Limits::default());
        let (started, failed) = interaction();
        trace.push_presence(
            epoch,
            presence("alice", PresenceChange::Interaction(Box::new(started))),
        );
        trace.push_presence(
            epoch,
            presence("alice", PresenceChange::Interaction(Box::new(failed))),
        );
        trace.push_op(
            epoch,
            route("so.tilo.notes"),
            "alice".into(),
            incremental(1),
        );
        trace.push_op(epoch, route("so.tilo.mail"), "alice".into(), incremental(2));
        trace.add_tag("send-fails");
        let trace = trace.finish(epoch + Duration::from_millis(1_500));
        let summary = trace.summary();
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.apps, vec!["so.tilo.mail", "so.tilo.notes"]);
        assert_eq!(summary.duration_ms, 1_500);
        let details = trace.details();
        assert_eq!(details.failed_steps, 1);
        assert_eq!(details.ops, 2);
        assert_eq!(details.rooms, vec!["room-1"]);
        assert_eq!(details.tags, vec!["send-fails"]);
    }
}
