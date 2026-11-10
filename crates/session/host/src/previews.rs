//! The page's live layer: the preview operations another member publishes
//! for a record.
//!
//! Presence is not here: a page's cursors, its shell's view, chat, reactions
//! and waves ride the workspace's presence plane
//! ([`crate::session::PeerEvent::LivePlane`]), the way every device's do.
//! This module keeps what the live plane still carries for the page —
//! ephemeral previews of a record's edits — and lets a publisher's previews
//! go stale when it leaves.

use std::collections::BTreeMap;

use tilo_relay_protocol::live::{LiveFrame, LiveRecord, OpsFrame, PreviewOp};

/// A publisher with no frame for this long is reported stale.
pub const STALE_AFTER_MS: u64 = 10_000;

/// One remote member as the renderer consumes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteMember {
    /// The member's device key.
    pub publisher: [u8; 32],
    /// Local receive time of the member's last frame (ms since the epoch).
    pub received_at_ms: u64,
    /// Pending preview operations per record, in arrival order.
    pub previews: BTreeMap<LiveRecord, Vec<PreviewOp>>,
    /// No frame for [`STALE_AFTER_MS`].
    pub stale: bool,
}

/// What an inbound frame changed.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveChange {
    /// A member's previews for a record changed.
    Preview([u8; 32], LiveRecord),
}

/// The live plane's previews of one workspace.
#[derive(Default)]
pub struct LiveState {
    remote: BTreeMap<[u8; 32], RemoteMember>,
}

impl LiveState {
    /// [`Self::default`] for a page starting now.
    pub fn starting_now() -> Self {
        Self::default()
    }

    /// Apply one frame the host relayed from `from`, received at `now_ms`.
    /// Returns what changed, or `None` for an undecodable frame or one that
    /// is not a preview.
    pub fn receive(&mut self, from: [u8; 32], frame: &[u8], now_ms: u64) -> Option<LiveChange> {
        let frame = LiveFrame::decode(frame).ok()?;
        let (record, ops) = match frame {
            LiveFrame::Ops(OpsFrame::Preview { record, ops, .. }) => (record, Some(ops)),
            LiveFrame::Ops(OpsFrame::End { record }) => (record, None),
            // An agent's narrated work is admitted by the page's presence
            // board, which knows its members: it never makes a member here
            // or keeps one fresh.
            LiveFrame::RetiredPresence | LiveFrame::Ops(OpsFrame::Semantic { .. }) => return None,
        };
        let member = self.remote.entry(from).or_insert_with(|| RemoteMember {
            publisher: from,
            received_at_ms: now_ms,
            previews: BTreeMap::new(),
            stale: false,
        });
        match ops {
            Some(ops) => {
                member
                    .previews
                    .entry(record.clone())
                    .or_default()
                    .extend(ops);
            }
            None => {
                member.previews.remove(&record);
            }
        }
        member.received_at_ms = now_ms;
        Some(LiveChange::Preview(from, record))
    }

    /// A durable change for `record` landed: previews for it are superseded.
    pub fn durable_changed(&mut self, record: &LiveRecord) {
        for member in self.remote.values_mut() {
            member.previews.remove(record);
        }
    }

    /// Every remote member at `now_ms`, with stale flags applied.
    pub fn remote(&mut self, now_ms: u64) -> Vec<RemoteMember> {
        for member in self.remote.values_mut() {
            member.stale = now_ms.saturating_sub(member.received_at_ms) >= STALE_AFTER_MS;
        }
        self.remote.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::live::LiveAnchor;

    fn record() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r".into(),
        }
    }

    fn preview(seq: u64) -> Vec<u8> {
        LiveFrame::Ops(OpsFrame::Preview {
            seq,
            record: record(),
            ops: vec![PreviewOp::Insert {
                at: LiveAnchor { cursor: vec![1] },
                text: "x".into(),
            }],
        })
        .encode()
        .unwrap()
    }

    #[test]
    fn semantic_events_refresh_no_member() {
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
        assert!(receiver.remote(1).is_empty());
        receiver.receive(from, &preview(1), 1);
        assert!(!receiver.remote(STALE_AFTER_MS)[0].stale);
        receiver.receive(from, &semantic, STALE_AFTER_MS);
        assert!(receiver.remote(STALE_AFTER_MS + 1)[0].stale);
    }

    #[test]
    fn previews_yield_to_durable_changes() {
        let mut receiver = LiveState::default();
        let from = [4; 32];
        assert!(receiver.receive(from, &preview(1), 10).is_some());
        assert_eq!(receiver.remote(14)[0].previews.len(), 1);
        receiver.durable_changed(&record());
        assert!(receiver.remote(15)[0].previews.is_empty());
        assert!(receiver.remote(13 + STALE_AFTER_MS)[0].stale);
    }

    #[test]
    fn a_retired_presence_frame_is_not_a_preview() {
        let mut receiver = LiveState::default();
        let frame = LiveFrame::RetiredPresence.encode().unwrap();
        assert_eq!(receiver.receive([4; 32], &frame, 1), None);
    }
}
