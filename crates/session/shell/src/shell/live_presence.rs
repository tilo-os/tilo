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
//! Selection anchors use the live plane's encoding of an editor key and
//! character offsets, [`selection_anchors`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use channel_crypto::ChannelContentKeys;
use collab_types::{ChannelId, PeerPresence, PeerPresencePayload};
use tilo_relay_protocol::live::{
    AnchorScope, CursorChat, LiveAnchor, LiveFrame, LiveParticipant, LiveReaction, LiveRecord,
    ParticipantKind, PresenceFrame, SealedLiveFrame, read_selection_anchors, selection_anchors,
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

/// Selection anchors for the editor whose postcard key is `key`. A key too
/// long for the anchor travels as its scope digest, which the receiving
/// editor resolves against its own keys.
fn anchors(key: &[u8], start: u64, end: u64) -> Option<(LiveAnchor, LiveAnchor)> {
    selection_anchors(
        key,
        || {
            postcard::from_bytes::<ui::presence::PresenceKey>(key)
                .ok()?
                .scope_digest()
        },
        start,
        end,
    )
}

/// The editor key and range of a pair made by [`anchors`]; a digest names
/// the key's digest stand-in.
fn read_anchors(anchor: &LiveAnchor, head: &LiveAnchor) -> Option<(Vec<u8>, u64, u64)> {
    let (scope, start, end) = read_selection_anchors(anchor, head)?;
    let key = match scope {
        AnchorScope::Key(key) => key.to_vec(),
        AnchorScope::Digest(digest) => {
            postcard::to_stdvec(&ui::presence::PresenceKey::from_scope_digest(digest)).ok()?
        }
    };
    Some((key, start, end))
}

struct Room {
    frame: PresenceFrame,
    sent: Instant,
    /// When the frame's reaction was sent: it is dropped from the frame
    /// once every receiver has had it ([`REACTION_HOLD`]).
    reacted: Option<Instant>,
    /// Reactions sent in this room: the counter receivers tell a new
    /// reaction from a restated one by.
    reactions: u32,
}

/// How long a reaction rides this device's frames: past the keepalive, so a
/// receiver that missed the first frame plays it from the repeat, and short
/// of a reaction's life, so nobody arriving later sees an old one.
pub const REACTION_HOLD: Duration = Duration::from_millis(1000);

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
        let room = self.room(ws, now);
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
        self.publish(ws, now)
    }

    fn room(&mut self, ws: u64, now: Instant) -> &mut Room {
        self.rooms.entry(ws).or_insert_with(|| Room {
            frame: PresenceFrame::default(),
            sent: now,
            reacted: None,
            reactions: 0,
        })
    }

    /// The room's frame with the next sequence number, encoded.
    fn publish(&mut self, ws: u64, now: Instant) -> Option<Vec<u8>> {
        let room = self.rooms.get_mut(&ws)?;
        self.seq = self.seq.wrapping_add(1);
        room.frame.seq = self.seq;
        room.frame.sent_at_ms = tilo_clock::unix_now_ms();
        room.sent = now;
        LiveFrame::Presence(room.frame.clone()).encode().ok()
    }

    /// Say `chat` beside this device's pointer at `pointer` (in the window
    /// showing `focus`, from its top-left corner) in the room whose workspace
    /// is `ws`, or stop saying anything with `None`. Returns the frame to
    /// publish; `None` for one the live plane refuses (a message past its
    /// bounds), which leaves what was said before.
    pub fn say(
        &mut self,
        ws: u64,
        at: (LiveRecord, [f32; 2]),
        participant: LiveParticipant,
        chat: Option<CursorChat>,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let (focus, pointer) = at;
        let room = self.room(ws, now);
        let before = room.frame.clone();
        room.frame.focus = Some(focus);
        room.frame.pointer = Some(pointer);
        room.frame.participant = Some(participant);
        room.frame.chat = chat;
        let frame = self.publish(ws, now);
        if frame.is_none()
            && let Some(room) = self.rooms.get_mut(&ws)
        {
            room.frame = before;
        }
        frame
    }

    /// React with `emoji` from this device's pointer, as [`Self::say`] says
    /// a message: the frame carries it, with a new counter, for
    /// [`REACTION_HOLD`].
    pub fn react(
        &mut self,
        ws: u64,
        at: (LiveRecord, [f32; 2]),
        participant: LiveParticipant,
        emoji: String,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let (focus, pointer) = at;
        let room = self.room(ws, now);
        let seq = room.reactions.wrapping_add(1);
        let before = room.frame.clone();
        room.frame.focus = Some(focus);
        room.frame.pointer = Some(pointer);
        room.frame.participant = Some(participant);
        room.frame.chat = None;
        room.frame.reaction = Some(LiveReaction { seq, emoji });
        room.reacted = Some(now);
        let frame = self.publish(ws, now);
        if let Some(room) = self.rooms.get_mut(&ws) {
            if frame.is_some() {
                room.reactions = seq;
            } else {
                room.frame = before;
                room.reacted = None;
            }
        }
        frame
    }

    /// Frames to repeat because they were not sent for [`KEEPALIVE`].
    pub fn keepalives(&mut self, now: Instant) -> Vec<(u64, Vec<u8>)> {
        let mut due = Vec::new();
        for (ws, room) in &mut self.rooms {
            // A reaction everyone has had leaves the frame with its next
            // statement, so a late arrival never plays an old one; the
            // room's counter stays, so the next reaction is still a new one.
            let reaction_over = room
                .reacted
                .is_some_and(|at| now.saturating_duration_since(at) >= REACTION_HOLD);
            if reaction_over {
                room.reacted = None;
                room.frame.reaction = None;
            }
            if (now.saturating_duration_since(room.sent) < KEEPALIVE && !reaction_over)
                || (room.frame.pointer.is_none()
                    && room.frame.selection.is_none()
                    && room.frame.chat.is_none())
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
    /// What the sender is saying beside its pointer, `None` for nothing.
    pub chat: Option<CursorChat>,
    /// The sender's latest reaction.
    pub reaction: Option<LiveReaction>,
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
        chat: presence.chat.clone(),
        reaction: presence.reaction.clone(),
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

    /// A key longer than an anchor (a resource key naming a long item id)
    /// crosses as its digest, filed under the stand-in key editors resolve.
    #[test]
    fn a_long_presence_key_crosses_as_its_digest() {
        let key = ui::presence::PresenceKey::Resource {
            app: "so.tilo.word-processor".into(),
            id: "c660cd1f77ac1ac5cb0bd0113c126523.0".into(),
            kind: "tilo.rich-text".into(),
        };
        let (anchor, head) = anchors(&postcard::to_stdvec(&key).unwrap(), 5, 5).unwrap();
        assert!(anchor.cursor.len() <= tilo_relay_protocol::live::MAX_OBJECT_ID_BYTES);
        let (read, start, end) = read_anchors(&anchor, &head).unwrap();
        assert_eq!(
            postcard::from_bytes::<ui::presence::PresenceKey>(&read).unwrap(),
            key.digest_form().unwrap()
        );
        assert_eq!((start, end), (5, 5));
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

    /// What a person types beside their pointer crosses the live plane with
    /// the window it is over and where in it, live while typed and then sent;
    /// a reaction rides the frame with a new counter, and leaves it once
    /// everyone has had it so nobody arriving later plays an old one.
    #[test]
    fn cursor_chat_and_reactions_cross_the_live_plane() {
        let mut outbound = Outbound::default();
        let now = Instant::now();
        let window = LiveRecord {
            app: "so.tilo.notes".into(),
            record: "note-1".into(),
        };
        let ada = || participant("Ada".into(), ui::tokens::PresenceHue::Teal);
        let typed = outbound
            .say(
                7,
                (window.clone(), [120.0, 48.0]),
                ada(),
                Some(CursorChat {
                    text: "look at th".into(),
                    sent: false,
                }),
                now,
            )
            .unwrap();
        let read = inbound(&typed).unwrap();
        assert_eq!(read.app_id, "so.tilo.notes");
        assert_eq!(read.label.as_deref(), Some("Ada"));
        assert_eq!(
            read.chat,
            Some(CursorChat {
                text: "look at th".into(),
                sent: false
            })
        );
        assert!(
            read.updates.iter().any(|update| matches!(
                update.payload,
                PeerPresencePayload::Pointer { x, y } if (x, y) == (120.0, 48.0)
            )),
            "the bubble hangs from the pointer it was typed at"
        );

        let sent = outbound
            .say(
                7,
                (window.clone(), [120.0, 48.0]),
                ada(),
                Some(CursorChat {
                    text: "look at this".into(),
                    sent: true,
                }),
                now,
            )
            .unwrap();
        assert!(inbound(&sent).unwrap().chat.unwrap().sent);
        // Kept alive while it shows, like a pointer.
        assert_eq!(outbound.keepalives(now + KEEPALIVE).len(), 1);

        // Past its bounds the live plane refuses it; what was said stays.
        let refused = outbound.say(
            7,
            (window.clone(), [120.0, 48.0]),
            ada(),
            Some(CursorChat {
                text: "x".repeat(tilo_relay_protocol::live::MAX_CURSOR_CHAT_CHARS + 1),
                sent: false,
            }),
            now,
        );
        assert!(refused.is_none());

        let reacted = outbound
            .react(
                7,
                (window.clone(), [120.0, 48.0]),
                ada(),
                "\u{1f389}".into(),
                now,
            )
            .unwrap();
        let read = inbound(&reacted).unwrap();
        assert_eq!(read.chat, None, "a reaction ends the message");
        let first = read.reaction.unwrap();
        assert_eq!(first.emoji, "\u{1f389}");
        let again = outbound
            .react(7, (window, [120.0, 48.0]), ada(), "\u{1f44d}".into(), now)
            .unwrap();
        assert_eq!(
            inbound(&again).unwrap().reaction.unwrap().seq,
            first.seq.wrapping_add(1),
            "each reaction is a new one"
        );

        let later = now + REACTION_HOLD;
        let restated = outbound.keepalives(later);
        assert_eq!(restated.len(), 1, "the reaction leaves the frame at once");
        assert_eq!(inbound(&restated[0].1).unwrap().reaction, None);
    }
}
