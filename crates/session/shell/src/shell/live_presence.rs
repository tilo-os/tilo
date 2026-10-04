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
//! A frame leaves only sealed with the content keys of the channel of the
//! record it names, and only a frame that opens with this device's keys is
//! read: [`seal`] and [`open`].
//!
//! Selection anchors use the browser peer's encoding: the editor key and a
//! character offset, `[ANCHOR_TAG_SCOPED, uvarint(start), postcard(key)]`
//! and `[ANCHOR_TAG_SAME_SCOPE, uvarint(end)]`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use channel_crypto::ChannelContentKeys;
use collab_types::{ChannelId, PeerPresence, PeerPresencePayload};
use tilo_relay_protocol::live::{
    ANCHOR_TAG_SAME_SCOPE, ANCHOR_TAG_SCOPED, LiveAnchor, LiveFrame, LiveParticipant, LiveRecord,
    MAX_OBJECT_ID_BYTES, ParticipantKind, PresenceFrame, SealedLiveFrame,
};

/// Seal canonical live `frame` of room `cluster_id` with the content keys of
/// the channel of the record it names; `keys` gives a channel's keys when
/// this device is admitted to it. `None` for a frame naming no record or
/// whose channel this device holds no keys for: it is never sent.
pub fn seal(
    cluster_id: &str,
    frame: &[u8],
    keys: impl FnOnce(ChannelId) -> Option<ChannelContentKeys>,
) -> Option<Vec<u8>> {
    let frame = LiveFrame::decode(frame).ok()?;
    let (_, _, record) = frame.seal_tags()?;
    let keys = keys(collab_types::document_channel_id(
        &record.app,
        cluster_id,
        &record.record,
    ))?;
    SealedLiveFrame::seal(&frame, |artifact, retention, cleartext| {
        keys.seal_content(artifact, retention, cleartext)
    })
    .ok()
}

/// Open sealed live `frame` with its channel's keys, returning the canonical
/// frame, or `None` when this device holds no keys that open it.
pub fn open(
    frame: &[u8],
    keys: impl FnOnce(ChannelId) -> Option<ChannelContentKeys>,
) -> Option<Vec<u8>> {
    let sealed = SealedLiveFrame::decode(frame).ok()?;
    let keys = keys(sealed.channel_id())?;
    sealed
        .open(|artifact, metadata, payload| {
            keys.open_content(artifact, metadata, payload)
                .map(|cleartext| cleartext.to_vec())
        })
        .ok()?
        .encode()
        .ok()
}

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
    /// The colour the sender asks to be drawn in: the presence hue nearest
    /// its participant colour.
    pub colour: Option<ui::tokens::PresenceHue>,
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
        colour: presence.participant.as_ref().and_then(|participant| {
            let [r, g, b] = participant.color;
            ui::tokens::PresenceHue::nearest(vello::peniko::Color::from_rgb8(r, g, b))
        }),
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

/// How this device asks to be drawn: under the person's name, in the
/// colour they wear everywhere (their choice, or the one their stable
/// identity derives).
pub fn participant(label: String, colour: ui::tokens::PresenceHue) -> LiveParticipant {
    let (r, g, b) = ui::presence::identity_rgb(colour);
    LiveParticipant {
        kind: ParticipantKind::Person,
        label,
        color: [r, g, b],
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

    /// A live frame leaves only sealed with the keys of its record's channel,
    /// opens only with them, and is never sealed without them.
    #[test]
    fn live_frames_leave_only_sealed_with_their_records_channel_keys() {
        let room = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
        let focus = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "note-1".into(),
        };
        let channel = collab_types::document_channel_id(&focus.app, room, &focus.record);
        let dir = tempfile::tempdir().unwrap();
        let keys = channel_crypto::ChannelSteward::open_or_create(
            dir.path(),
            channel,
            ed25519_dalek::SigningKey::from_bytes(&[4; 32]),
            &channel_crypto::VaultKey::new([9; 32]),
        )
        .unwrap()
        .content_keys()
        .unwrap();
        let mut live = Outbound::default();
        let frame = live
            .update(
                1,
                focus,
                participant("Ada".into(), ui::tokens::PresenceHue::Teal),
                &PeerPresence {
                    key: caret_key(),
                    payload: PeerPresencePayload::Caret { pos: 3 },
                },
                Instant::now(),
            )
            .unwrap();
        let held = |wanted| (wanted == channel).then(|| keys.clone());

        let sealed = seal(room, &frame, held).unwrap();
        assert!(LiveFrame::decode(&sealed).is_err());
        assert!(
            !sealed.windows(frame.len()).any(|window| window == frame),
            "nothing of the frame is in the clear"
        );
        assert_eq!(open(&sealed, held).unwrap(), frame);

        assert!(seal(room, &frame, |_| None).is_none(), "no keys: not sent");
        assert!(open(&sealed, |_| None).is_none());
        assert!(
            open(&frame, held).is_none(),
            "a frame in the clear is not read"
        );
        let other_room = "0d6a7e0c-3b0a-4a64-9a39-6e9b6f1f2a10";
        assert!(
            seal(other_room, &frame, held).is_none(),
            "another room's channel has other keys"
        );
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
                participant("Ada".into(), ui::tokens::PresenceHue::Teal),
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
            read.colour,
            Some(ui::tokens::PresenceHue::Teal),
            "the person's colour crosses the live plane"
        );
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
                participant("Ada".into(), ui::tokens::PresenceHue::Teal),
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
