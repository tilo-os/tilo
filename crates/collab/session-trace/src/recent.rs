//! The retroactive buffer: what just happened, in memory only.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use collab_types::{ArtifactId, Ed25519PublicKey, SignedOp};

use crate::{Event, InspectKeyframe, Limits, Origin, PresenceUpdate, Route, TraceBuilder};

/// Names one signed op without holding it. The op itself stays in the
/// record's history, where the broker's validated-op log can serve it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpReference {
    pub route: Route,
    pub from: String,
    pub artifact: ArtifactId,
    pub author: Ed25519PublicKey,
    pub seq: u64,
}

impl OpReference {
    pub fn of(route: Route, from: String, op: &SignedOp) -> Self {
        Self {
            route,
            from,
            artifact: op.artifact,
            author: op.author,
            seq: op.seq,
        }
    }

    /// Whether `op` is the op this names.
    pub fn names(&self, op: &SignedOp) -> bool {
        op.artifact == self.artifact && op.author == self.author && op.seq == self.seq
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RecentItem {
    Presence(PresenceUpdate),
    Op(OpReference),
    Inspect(InspectKeyframe),
}

impl RecentItem {
    fn weight(&self) -> usize {
        match self {
            Self::Presence(update) => Event::Presence(update.clone()).weight(),
            Self::Inspect(frame) => frame.answer_weight().saturating_add(192),
            Self::Op(reference) => reference
                .from
                .len()
                .saturating_add(reference.route.app_id.len())
                .saturating_add(reference.route.record_id.len())
                .saturating_add(128),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecentEntry {
    pub at: Instant,
    pub item: RecentItem,
}

/// A ring of the last `window` of presence, inspect keyframes and op
/// references, bounded by age and by bytes. It has no persistence and no
/// encoding: it cannot be written or sent, only turned into a trace.
pub struct RecentBuffer {
    window: Duration,
    max_bytes: usize,
    bytes: usize,
    entries: VecDeque<RecentEntry>,
}

impl RecentBuffer {
    /// Two minutes, per the feedback design.
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(120);
    pub const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024;

    pub fn new(window: Duration, max_bytes: usize) -> Self {
        Self {
            window,
            max_bytes,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &RecentEntry> {
        self.entries.iter()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Append, then evict whatever is older than the window or over budget.
    /// An entry timestamped before the newest one is placed at the newest
    /// time, so the ring stays ordered.
    pub fn push(&mut self, at: Instant, item: RecentItem) {
        let at = self.entries.back().map_or(at, |last| at.max(last.at));
        self.bytes = self.bytes.saturating_add(item.weight());
        self.entries.push_back(RecentEntry { at, item });
        self.evict(at);
    }

    /// Drop entries older than the window at `now`, and the oldest entries
    /// while over the byte budget.
    pub fn evict(&mut self, now: Instant) {
        while let Some(front) = self.entries.front() {
            let expired = now.saturating_duration_since(front.at) > self.window;
            if !expired && self.bytes <= self.max_bytes {
                break;
            }
            if let Some(gone) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(gone.item.weight());
            }
        }
    }

    /// Build a retroactive trace of the window ending at `now`. `resolve`
    /// fetches a referenced op from history; a reference it cannot serve
    /// (evicted by compaction) is skipped. Returns the builder so the
    /// caller can add the end-of-window snapshots before finishing.
    pub fn capture(
        &mut self,
        now: Instant,
        wall_now: SystemTime,
        limits: Limits,
        mut resolve: impl FnMut(&OpReference) -> Option<SignedOp>,
    ) -> TraceBuilder {
        self.evict(now);
        let epoch = self.entries.front().map_or(now, |first| first.at);
        let started = wall_now
            .checked_sub(now.saturating_duration_since(epoch))
            .unwrap_or(wall_now);
        let mut builder = TraceBuilder::new(Origin::Retroactive, epoch, started, limits);
        for entry in &self.entries {
            match &entry.item {
                RecentItem::Presence(update) => {
                    builder.push_presence(entry.at, update.clone());
                }
                RecentItem::Inspect(frame) => {
                    builder.push_inspect(entry.at, frame.clone());
                }
                RecentItem::Op(reference) => {
                    if let Some(op) = resolve(reference).filter(|op| reference.names(op)) {
                        builder.push_timeline_op(
                            entry.at,
                            reference.route.clone(),
                            reference.from.clone(),
                            op,
                        );
                    }
                }
            }
        }
        builder
    }
}

impl Default for RecentBuffer {
    fn default() -> Self {
        Self::new(Self::DEFAULT_WINDOW, Self::DEFAULT_MAX_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{incremental, presence, route, snapshot};
    use crate::{Event, PresenceChange, Pushed, SnapshotPoint};

    #[test]
    fn ring_keeps_only_the_window_and_the_budget() {
        let start = Instant::now();
        let mut ring = RecentBuffer::new(Duration::from_secs(120), usize::MAX);
        ring.push(
            start,
            RecentItem::Presence(presence("a", PresenceChange::Arrived)),
        );
        ring.push(
            start + Duration::from_secs(60),
            RecentItem::Presence(presence("a", PresenceChange::Caret { pos: 1 })),
        );
        ring.push(
            start + Duration::from_secs(150),
            RecentItem::Presence(presence("a", PresenceChange::Caret { pos: 2 })),
        );
        assert_eq!(ring.len(), 2, "the first entry aged out");
        ring.evict(start + Duration::from_secs(400));
        assert!(ring.is_empty());

        let mut small = RecentBuffer::new(Duration::from_secs(120), 600);
        for pos in 0..10 {
            small.push(
                start,
                RecentItem::Presence(presence("a", PresenceChange::Caret { pos })),
            );
        }
        assert!(small.len() < 10 && !small.is_empty());
        let newest = small.entries().last().expect("newest kept");
        assert!(matches!(
            &newest.item,
            RecentItem::Presence(update) if update.change == PresenceChange::Caret { pos: 9 }
        ));
    }

    #[test]
    fn capture_resolves_op_references_from_history_and_takes_end_snapshots() {
        let start = Instant::now();
        let mut ring = RecentBuffer::default();
        let notes = route("so.tilo.notes");
        let kept = incremental(7);
        let evicted = incremental(8);
        ring.push(
            start,
            RecentItem::Op(OpReference::of(notes.clone(), "alice".into(), &kept)),
        );
        ring.push(
            start + Duration::from_secs(1),
            RecentItem::Presence(presence("alice", PresenceChange::Caret { pos: 3 })),
        );
        ring.push(
            start + Duration::from_secs(2),
            RecentItem::Op(OpReference::of(notes.clone(), "alice".into(), &evicted)),
        );
        let history = [kept.clone()];
        let mut trace = ring.capture(
            start + Duration::from_secs(3),
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            Limits::default(),
            |reference| history.iter().find(|op| reference.names(op)).cloned(),
        );
        assert_eq!(
            trace.push_op(
                start + Duration::from_secs(3),
                notes,
                "alice".into(),
                snapshot(9)
            ),
            Pushed::Snapshot
        );
        let trace = trace.finish(start + Duration::from_secs(3));
        assert_eq!(trace.origin, Origin::Retroactive);
        assert_eq!(trace.started_unix_ms, 97_000);
        assert_eq!(trace.snapshots[0].point, SnapshotPoint::End);
        assert_eq!(
            trace.timeline.len(),
            2,
            "the op history could not serve is skipped"
        );
        assert!(matches!(&trace.timeline[0].event, Event::Op(op) if op.envelope == kept));
        assert_eq!(trace.timeline[1].at.as_millis(), 1_000);
        assert_eq!(trace.duration.as_millis(), 3_000);
    }
}
