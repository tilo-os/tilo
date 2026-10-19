//! The live layer: what this device publishes and what it believes about
//! every other live member — the Rust half of PR 541's `LIVE_V1` receiver.
//!
//! Presence is latest-value: a frame older than the newest one from the
//! same publisher is dropped. Previews are ordered per publisher and record
//! and are an overlay: they are dropped when the publisher ends them, when
//! the record's durable change lands ([`LiveState::durable_changed`]), or
//! when the publisher leaves. A released drag stays until the durable change
//! lands or [`ENDED_DRAG_TIMEOUT_MS`] passes. Nothing here is authority;
//! interpolation of remote pointers belongs to the renderer
//! (`web/live-presence.mjs`).

use std::collections::{BTreeMap, BTreeSet};

use tilo_relay_protocol::live::{
    DragPhase, LiveFrame, LiveRecord, LiveWireError, OpsFrame, PresenceFrame, PreviewOp,
};

/// A released drag is shown at most this long without its durable change.
pub const ENDED_DRAG_TIMEOUT_MS: u64 = 5_000;
/// A publisher with no frame for this long is reported stale.
pub const STALE_AFTER_MS: u64 = 10_000;

/// One remote member as the renderer consumes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteMember {
    /// The member's device key.
    pub publisher: [u8; 32],
    /// Its newest presence, if any arrived.
    pub presence: Option<PresenceFrame>,
    /// Local receive time of that presence (ms since the epoch).
    pub received_at_ms: u64,
    /// Released drag kept until its durable change lands, with its release
    /// time.
    pub ended_drag: Option<(PresenceFrame, u64)>,
    /// Pending preview operations per record, in arrival order.
    pub previews: BTreeMap<LiveRecord, Vec<PreviewOp>>,
    /// No frame for [`STALE_AFTER_MS`].
    pub stale: bool,
}

/// What an inbound frame changed.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveChange {
    /// A member's presence moved.
    Presence([u8; 32]),
    /// A member's previews for a record changed.
    Preview([u8; 32], LiveRecord),
    /// Membership changed.
    Member([u8; 32], bool),
}

/// Local and remote live state of one room.
#[derive(Default)]
pub struct LiveState {
    members: BTreeSet<[u8; 32]>,
    remote: BTreeMap<[u8; 32], RemoteMember>,
    local_presence: Option<PresenceFrame>,
    presence_seq: u64,
}

impl LiveState {
    /// State for a publisher starting at `now_ms`. Receivers keep only
    /// strictly newer presence, so the sequence starts from the wall clock
    /// (microseconds) to stay ahead of this page's previous incarnation.
    pub fn new(now_ms: u64) -> Self {
        Self {
            presence_seq: now_ms.saturating_mul(1000),
            ..Self::default()
        }
    }

    /// [`Self::new`] at the current wall-clock time.
    pub fn starting_now() -> Self {
        Self::new(tilo_clock::unix_now_ms())
    }

    /// A member joined or left the room's live plane.
    pub fn member(&mut self, device: [u8; 32], active: bool) -> LiveChange {
        if active {
            self.members.insert(device);
        } else {
            self.members.remove(&device);
            self.remote.remove(&device);
        }
        LiveChange::Member(device, active)
    }

    /// Current live members other than this device.
    pub fn members(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.members.iter()
    }

    /// Apply one frame the host relayed from `from`, received at `now_ms`.
    /// Returns what changed, or `None` for a stale or undecodable frame.
    pub fn receive(&mut self, from: [u8; 32], frame: &[u8], now_ms: u64) -> Option<LiveChange> {
        let frame = LiveFrame::decode(frame).ok()?;
        // Semantic work requires a separately authenticated consumer. It must
        // never create a pointer member or refresh a pointer's idle lifetime.
        if matches!(frame, LiveFrame::Ops(OpsFrame::Semantic { .. })) {
            return None;
        }
        self.members.insert(from);
        let member = self.remote.entry(from).or_insert_with(|| RemoteMember {
            publisher: from,
            presence: None,
            received_at_ms: now_ms,
            ended_drag: None,
            previews: BTreeMap::new(),
            stale: false,
        });
        match frame {
            LiveFrame::Presence(presence) => {
                if member
                    .presence
                    .as_ref()
                    .is_some_and(|newest| newest.seq >= presence.seq)
                {
                    return None;
                }
                if let Some((_, DragPhase::End, _, _)) = &presence.drag {
                    member.ended_drag = Some((presence.clone(), now_ms));
                }
                member.presence = Some(presence);
                member.received_at_ms = now_ms;
                member.stale = false;
                Some(LiveChange::Presence(from))
            }
            LiveFrame::Ops(OpsFrame::Preview { record, ops, .. }) => {
                member
                    .previews
                    .entry(record.clone())
                    .or_default()
                    .extend(ops);
                member.received_at_ms = now_ms;
                Some(LiveChange::Preview(from, record))
            }
            LiveFrame::Ops(OpsFrame::End { record }) => {
                member.previews.remove(&record);
                Some(LiveChange::Preview(from, record))
            }
            LiveFrame::Ops(OpsFrame::Semantic { .. }) => None,
        }
    }

    /// A durable change for `record` landed: previews for it are superseded
    /// and a released drag on it snaps to the durable position.
    pub fn durable_changed(&mut self, record: &LiveRecord) {
        for member in self.remote.values_mut() {
            member.previews.remove(record);
            if member.ended_drag.as_ref().is_some_and(|(presence, _)| {
                presence
                    .drag
                    .as_ref()
                    .is_some_and(|(object, ..)| object.record.as_ref() == Some(record))
            }) {
                member.ended_drag = None;
            }
        }
    }

    /// Every remote member at `now_ms`, with stale flags and expired
    /// released drags applied.
    pub fn remote(&mut self, now_ms: u64) -> Vec<RemoteMember> {
        for member in self.remote.values_mut() {
            member.stale = now_ms.saturating_sub(member.received_at_ms) >= STALE_AFTER_MS;
            if member
                .ended_drag
                .as_ref()
                .is_some_and(|(_, at)| now_ms.saturating_sub(*at) >= ENDED_DRAG_TIMEOUT_MS)
            {
                member.ended_drag = None;
            }
        }
        self.remote.values().cloned().collect()
    }

    /// Merge a local presence update and encode the frame to publish.
    /// `update` receives the current presence to modify.
    ///
    /// # Errors
    ///
    /// Returns an error when the result violates the wire bounds.
    pub fn publish_presence(
        &mut self,
        now_ms: u64,
        update: impl FnOnce(&mut PresenceFrame),
    ) -> Result<Vec<u8>, LiveWireError> {
        let mut presence = self.local_presence.clone().unwrap_or(PresenceFrame {
            place: None,
            chat: None,
            reaction: None,
            seq: 0,
            sent_at_ms: 0,
            focus: None,
            pointer: None,
            selection: None,
            drag: None,
            participant: None,
        });
        update(&mut presence);
        self.presence_seq = self.presence_seq.saturating_add(1);
        presence.seq = self.presence_seq;
        presence.sent_at_ms = now_ms;
        let encoded = LiveFrame::Presence(presence.clone()).encode()?;
        self.local_presence = Some(presence);
        Ok(encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::live::{LiveAnchor, LiveObject};

    fn record() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r".into(),
        }
    }

    #[test]
    fn semantic_events_cannot_create_or_refresh_pointer_presence() {
        let from = [4; 32];
        let semantic = LiveFrame::Ops(OpsFrame::Semantic {
            record: record(),
            event: tilo_relay_protocol::live::SemanticFrame {
                version: 1,
                seq: 1,
                sent_at_ms: 1,
                execution: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
                peer: 7,
                interaction_json: "{}".into(),
            },
        })
        .encode()
        .unwrap();
        let mut receiver = LiveState::default();
        assert_eq!(receiver.receive(from, &semantic, 1), None);
        assert_eq!(receiver.members().count(), 0);
        assert!(receiver.remote(1).is_empty());
        let pointer = LiveState::default()
            .publish_presence(1, |presence| {
                presence.focus = Some(record());
                presence.pointer = Some([10.0, 20.0]);
            })
            .unwrap();
        receiver.receive(from, &pointer, 1);
        receiver.receive(from, &semantic, STALE_AFTER_MS);
        assert!(receiver.remote(STALE_AFTER_MS + 1)[0].stale);
    }

    #[test]
    fn older_presence_is_dropped_and_previews_yield_to_durable_changes() {
        let mut sender = LiveState::default();
        let mut receiver = LiveState::default();
        let first = sender
            .publish_presence(1, |p| p.pointer = Some([1.0, 1.0]))
            .unwrap();
        let second = sender
            .publish_presence(2, |p| p.pointer = Some([2.0, 2.0]))
            .unwrap();
        let from = [4; 32];
        assert!(receiver.receive(from, &second, 10).is_some());
        assert!(receiver.receive(from, &first, 11).is_none(), "older frame");
        assert_eq!(
            receiver.remote(12)[0]
                .presence
                .as_ref()
                .and_then(|p| p.pointer),
            Some([2.0, 2.0])
        );

        let preview = LiveFrame::Ops(OpsFrame::Preview {
            seq: 1,
            record: record(),
            ops: vec![PreviewOp::Insert {
                at: LiveAnchor { cursor: vec![1] },
                text: "x".into(),
            }],
        })
        .encode()
        .unwrap();
        receiver.receive(from, &preview, 13);
        assert_eq!(receiver.remote(14)[0].previews.len(), 1);
        receiver.durable_changed(&record());
        assert!(receiver.remote(15)[0].previews.is_empty());
        assert!(receiver.remote(13 + STALE_AFTER_MS)[0].stale);
    }

    #[test]
    fn released_drag_waits_for_the_durable_change() {
        let mut sender = LiveState::default();
        let mut receiver = LiveState::default();
        let end = sender
            .publish_presence(1, |p| {
                p.drag = Some((
                    LiveObject {
                        record: Some(record()),
                        id: vec![7],
                    },
                    DragPhase::End,
                    [5.0, 5.0],
                    None,
                ));
            })
            .unwrap();
        receiver.receive([4; 32], &end, 100);
        assert!(receiver.remote(200)[0].ended_drag.is_some());
        receiver.durable_changed(&record());
        assert!(receiver.remote(201)[0].ended_drag.is_none());
        receiver.receive([4; 32], &end, 300);
        receiver.member([4; 32], false);
        assert!(receiver.remote(301).is_empty());
    }
}
