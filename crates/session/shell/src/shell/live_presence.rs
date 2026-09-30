//! The shell on its workspace hosts' live plane.
//!
//! Browsers and agents publish pointers, carets and selections as
//! `tilo_relay_protocol::live` presence frames; the host relays them to every
//! live member, native shells included. This module translates between that
//! vocabulary and the shell's own presence (`collab_types::PeerPresence`,
//! keyed by the editor's `PresenceKey`), so named cursors cross between
//! native apps and browsers in both directions:
//!
//! - outbound, each local presence update folds into the room's latest
//!   frame (focus record, pointer, selection, the person drawn), which is
//!   published whole with the next sequence number and repeated as a
//!   keep-alive so a quiet cursor does not go stale on other devices;
//! - inbound, a frame becomes the pointer and caret/selection updates the
//!   broker fans out to local apps, and its participant names the sender.
//!
//! Selection anchors use the browser peer's encoding: the editor key and a
//! character offset, `[ANCHOR_TAG_SCOPED, uvarint(start), postcard(key)]`
//! and `[ANCHOR_TAG_SAME_SCOPE, uvarint(end)]`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use collab_types::{PeerPresence, PeerPresencePayload};
use tilo_relay_protocol::live::{
    ANCHOR_TAG_SAME_SCOPE, ANCHOR_TAG_SCOPED, LiveAnchor, LiveFrame, LiveParticipant, LiveRecord,
    MAX_OBJECT_ID_BYTES, ParticipantKind, PresenceFrame,
};

/// A quiet cursor is repeated this often so other devices keep drawing it
/// (browsers mark a peer stale after 10 s without a frame).
pub const KEEPALIVE: Duration = Duration::from_secs(3);

fn push_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (index, (byte, shift)) in bytes.iter().zip((0u32..64).step_by(7)).enumerate() {
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, bytes.get(index.checked_add(1)?..)?));
        }
    }
    None
}

/// Selection anchors for the editor whose postcard key is `key`.
fn anchors(key: &[u8], start: u64, end: u64) -> Option<(LiveAnchor, LiveAnchor)> {
    let mut anchor = vec![ANCHOR_TAG_SCOPED];
    push_uvarint(&mut anchor, start);
    anchor.extend_from_slice(key);
    let mut head = vec![ANCHOR_TAG_SAME_SCOPE];
    push_uvarint(&mut head, end);
    (anchor.len() <= MAX_OBJECT_ID_BYTES)
        .then_some((LiveAnchor { cursor: anchor }, LiveAnchor { cursor: head }))
}

/// The editor key and range of a pair made by [`anchors`].
fn read_anchors(anchor: &LiveAnchor, head: &LiveAnchor) -> Option<(Vec<u8>, u64, u64)> {
    let (&tag, rest) = anchor.cursor.split_first()?;
    if tag != ANCHOR_TAG_SCOPED {
        return None;
    }
    let (start, key) = read_uvarint(rest)?;
    let (&tag, rest) = head.cursor.split_first()?;
    if tag != ANCHOR_TAG_SAME_SCOPE {
        return None;
    }
    let (end, _) = read_uvarint(rest)?;
    Some((key.to_vec(), start, end))
}

/// A stable cursor colour for a device.
pub fn color_of(device: &[u8; 32]) -> [u8; 3] {
    const PALETTE: [[u8; 3]; 8] = [
        [0xe0, 0x4f, 0x5f],
        [0x2f, 0x80, 0xed],
        [0x27, 0xae, 0x60],
        [0xf2, 0x99, 0x4a],
        [0x9b, 0x51, 0xe0],
        [0x00, 0xa3, 0xa3],
        [0xd6, 0x33, 0x84],
        [0x82, 0x82, 0x2d],
    ];
    let index = usize::from(device.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte))) % 8;
    PALETTE.get(index).copied().unwrap_or([0x80, 0x80, 0x80])
}

struct Room {
    frame: PresenceFrame,
    sent: Instant,
}

/// This device's latest presence per room, as live frames.
#[derive(Default)]
pub struct Outbound {
    rooms: HashMap<u64, Room>,
    seq: u64,
}

impl Outbound {
    /// Fold one local presence update of `app`'s `record` in the room whose
    /// workspace is `ws` into its frame; returns the encoded frame to
    /// publish, or `None` for an update the live plane has no field for.
    pub fn update(
        &mut self,
        ws: u64,
        focus: LiveRecord,
        participant: LiveParticipant,
        presence: &PeerPresence,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let room = self.rooms.entry(ws).or_insert_with(|| Room {
            frame: PresenceFrame {
                seq: 0,
                sent_at_ms: 0,
                focus: None,
                pointer: None,
                selection: None,
                drag: None,
                participant: None,
            },
            sent: now,
        });
        let frame = &mut room.frame;
        match &presence.payload {
            PeerPresencePayload::Pointer { x, y } => frame.pointer = Some([*x, *y]),
            PeerPresencePayload::Caret { pos } => {
                frame.selection = anchors(&presence.key, *pos, *pos);
            }
            PeerPresencePayload::Selection { start, end } => {
                frame.selection = anchors(&presence.key, *start, *end);
            }
            PeerPresencePayload::Left => {
                frame.pointer = None;
                frame.selection = None;
            }
            PeerPresencePayload::Arrived => {}
            _ => return None,
        }
        frame.focus = Some(focus);
        frame.participant = Some(participant);
        self.seq = self.seq.wrapping_add(1);
        frame.seq = self.seq;
        frame.sent_at_ms = tilo_clock::unix_now_ms();
        room.sent = now;
        LiveFrame::Presence(frame.clone()).encode().ok()
    }

    /// Frames to repeat because they were not sent for [`KEEPALIVE`].
    pub fn keepalives(&mut self, now: Instant) -> Vec<(u64, Vec<u8>)> {
        let mut due = Vec::new();
        for (ws, room) in &mut self.rooms {
            if now.saturating_duration_since(room.sent) < KEEPALIVE
                || (room.frame.pointer.is_none() && room.frame.selection.is_none())
            {
                continue;
            }
            self.seq = self.seq.wrapping_add(1);
            room.frame.seq = self.seq;
            room.frame.sent_at_ms = tilo_clock::unix_now_ms();
            room.sent = now;
            if let Ok(bytes) = LiveFrame::Presence(room.frame.clone()).encode() {
                due.push((*ws, bytes));
            }
        }
        due
    }
}

/// What one inbound live frame means to local apps.
#[derive(Debug, Default, PartialEq)]
pub struct Inbound {
    /// The app the sender is focused on, when it said.
    pub app_id: String,
    /// The pointer, then the caret or selection, as local presence.
    pub updates: Vec<PeerPresence>,
    /// The name the sender asks to be drawn with.
    pub label: Option<String>,
}

/// Translate one live frame. Previews and malformed frames mean nothing
/// here: `None`.
pub fn inbound(frame: &[u8]) -> Option<Inbound> {
    let LiveFrame::Presence(presence) = LiveFrame::decode(frame).ok()? else {
        return None;
    };
    let mut inbound = Inbound {
        app_id: presence
            .focus
            .as_ref()
            .map(|focus| focus.app.clone())
            .unwrap_or_default(),
        label: presence
            .participant
            .as_ref()
            .map(|participant| participant.label.clone()),
        ..Inbound::default()
    };
    if let Some([x, y]) = presence.pointer {
        inbound.updates.push(PeerPresence {
            key: postcard::to_stdvec(&ui::presence::PresenceKey::pointer_default()).ok()?,
            payload: PeerPresencePayload::Pointer { x, y },
        });
    }
    if let Some((key, start, end)) = presence
        .selection
        .as_ref()
        .and_then(|(anchor, head)| read_anchors(anchor, head))
    {
        inbound.updates.push(PeerPresence {
            key,
            payload: if start == end {
                PeerPresencePayload::Caret { pos: start }
            } else {
                PeerPresencePayload::Selection { start, end }
            },
        });
    }
    if presence.pointer.is_none() && presence.selection.is_none() {
        inbound.updates.push(PeerPresence {
            key: postcard::to_stdvec(&ui::presence::PresenceKey::pointer_default()).ok()?,
            payload: PeerPresencePayload::Left,
        });
    }
    Some(inbound)
}

/// How this device asks to be drawn.
pub fn participant(label: String, device: &[u8; 32]) -> LiveParticipant {
    LiveParticipant {
        kind: ParticipantKind::Person,
        label,
        color: color_of(device),
        activity: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret_key() -> Vec<u8> {
        postcard::to_stdvec(&ui::presence::PresenceKey::Widget {
            path: vec![ui::presence::PathSegment::Field("editor".into())],
        })
        .unwrap()
    }

    /// A native caret published as a live frame reads back, name included,
    /// as the same caret in the same editor: what a browser draws, and what
    /// a native shell turns a browser's frame into.
    #[test]
    fn a_caret_crosses_the_live_plane_with_its_editor_and_name() {
        let mut outbound = Outbound::default();
        let now = Instant::now();
        let focus = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "derived:default@tab:0000000000000000".into(),
        };
        let frame = outbound
            .update(
                7,
                focus.clone(),
                participant("Ada".into(), &[3; 32]),
                &PeerPresence {
                    key: caret_key(),
                    payload: PeerPresencePayload::Caret { pos: 42 },
                },
                now,
            )
            .unwrap();
        let LiveFrame::Presence(decoded) = LiveFrame::decode(&frame).unwrap() else {
            panic!("a presence frame");
        };
        assert_eq!(decoded.focus, Some(focus));
        assert_eq!(decoded.participant.unwrap().label, "Ada");

        let read = inbound(&frame).unwrap();
        assert_eq!(read.app_id, "so.tilo.notes");
        assert_eq!(read.label.as_deref(), Some("Ada"));
        assert_eq!(
            read.updates,
            vec![PeerPresence {
                key: caret_key(),
                payload: PeerPresencePayload::Caret { pos: 42 },
            }]
        );

        // A pointer joins the caret in the next frame; both come back.
        let frame = outbound
            .update(
                7,
                LiveRecord {
                    app: "so.tilo.notes".into(),
                    record: "r".into(),
                },
                participant("Ada".into(), &[3; 32]),
                &PeerPresence {
                    key: postcard::to_stdvec(&ui::presence::PresenceKey::pointer_default())
                        .unwrap(),
                    payload: PeerPresencePayload::Pointer { x: 10.0, y: 20.0 },
                },
                now,
            )
            .unwrap();
        assert_eq!(inbound(&frame).unwrap().updates.len(), 2);
        assert!(outbound.keepalives(now).is_empty());
        assert_eq!(outbound.keepalives(now + KEEPALIVE).len(), 1);
    }
}
