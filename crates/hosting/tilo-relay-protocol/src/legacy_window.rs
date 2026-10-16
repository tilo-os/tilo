//! Legacy (non-Tilo) windows a session streams as video, carried to browser
//! members on the live plane.
//!
//! A session shares a legacy window with the room's host by naming the
//! host's device among the stream's viewers: it sends the host a
//! `PermissionGrant` for `workspace-control:<surface>:granted`, then the
//! stream's `SurfaceStreamStart`, `SurfaceFrame`s and `SurfaceStreamEnd`
//! as presence of its own device. The host re-publishes such a stream on
//! ordered record tracks in that device's live namespace, where the live
//! grant decides who receives it. Only the host writes these tracks: a
//! member's ordered frame must decode as an [`OpsFrame`](crate::live::OpsFrame),
//! and every frame here starts with [`FRAME_TAG`], which never begins one.
//!
//! * The index record ([`index_record`]) carries one [`Frame::Index`] listing
//!   the device's current streams, re-sent while any stream is live.
//! * Each stream has its own record ([`stream_record`]). One group per coded
//!   video sequence: [`Frame::Start`], then the keyframe's [`Frame::Chunk`]s,
//!   then later frames' chunks, so a subscriber that starts at the latest
//!   group can decode at once. An encoded frame larger than one ordered frame
//!   is split into consecutive chunks of that group. [`Frame::End`] closes it.
//!
//! A member that may drive the window sends [`InputRequest`]s on the HTTPS
//! route [`INPUT_ROUTE`]; the host forwards each event to the session as
//! `RemoteInput` of its own device, the viewer the session granted.
//!
//! A session picks the stream's codec from what its viewers advertised they
//! decode, and the host is the viewer for every browser member. Each page
//! says which [`VIDEO_CODECS`] its `WebCodecs` decodes in a [`CodecsRequest`]
//! on [`CODECS_ROUTE`]; the host advertises to the room's sessions what
//! every member currently in the live plane decodes, a member that never
//! said counting as [`H264`] only. So a session that cannot encode H.264
//! (no hardware encoder, no codec pack) still streams [`VP9`] or [`AV1`]
//! when every member takes it, and H.264 stays the choice whenever the
//! session can encode it.

use serde::{Deserialize, Serialize};

use crate::live::{LiveRecord, LiveWireError, MAX_OPS_FRAME_BYTES};

/// Application id of every legacy-window record.
pub const APP: &str = "so.tilo.legacy-window";
/// Record id of a device's stream index.
pub const INDEX_RECORD: &str = "@tilo:legacy-windows";
/// First byte of every frame; `0xF1` is never the first byte of a live frame.
pub const FRAME_TAG: u8 = 0xF1;
const KIND_INDEX: u8 = 1;
const KIND_START: u8 = 2;
const KIND_CHUNK: u8 = 3;
const KIND_END: u8 = 4;
/// Chunk flag: the frame is a keyframe.
pub const CHUNK_KEY: u8 = 1;
/// Chunk flag: first chunk of an encoded frame.
pub const CHUNK_FIRST: u8 = 2;
/// Chunk flag: last chunk of an encoded frame.
pub const CHUNK_LAST: u8 = 4;
const CHUNK_HEADER: usize = 2 + 1 + 4 + 8 + 4;
/// Encoded bytes per chunk, so every chunk fits one ordered frame.
pub const MAX_CHUNK_DATA: usize = MAX_OPS_FRAME_BYTES - CHUNK_HEADER;
/// Largest encoded frame the host forwards; larger ones are dropped.
pub const MAX_ENCODED_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Streams one index may list.
pub const MAX_STREAMS: usize = 32;
/// The capability a session grants the host to watch (and drive) `surface`.
pub const GRANT_PREFIX: &str = "workspace-control:";
/// Route (below [`crate::web::API_PREFIX`]) a member sends input on.
pub const INPUT_ROUTE: &str = "/live/legacy-input";
/// Events one input request may carry.
pub const MAX_INPUT_EVENTS: usize = 64;
/// Largest pasted text one event carries.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
/// Largest committed input-method text one event carries: the session's
/// `MAX_REMOTE_TEXT_BYTES`, so the host never forwards text its input
/// firewall refuses.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Route (below [`crate::web::API_PREFIX`]) a member says what it decodes on.
pub const CODECS_ROUTE: &str = "/live/legacy-codecs";
/// Labels one [`CodecsRequest`] may carry.
pub const MAX_CODEC_LABELS: usize = 8;
/// Longest label one [`CodecsRequest`] may carry.
pub const MAX_CODEC_LABEL_BYTES: usize = 32;

/// One input event for a legacy window: the session's remote-input
/// vocabulary (`RemoteInput::event`), variant for variant and in its order,
/// so its postcard encoding is the one the session decodes. Coordinates are
/// surface-local pixels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// The pointer moved to surface-local `(x, y)`.
    PointerMotion {
        /// Surface-local x, pixels.
        x: f64,
        /// Surface-local y, pixels.
        y: f64,
    },
    /// A pointer button changed.
    PointerButton {
        /// Linux input code (272 = left).
        button: u32,
        /// Pressed or released.
        pressed: bool,
    },
    /// The pointer entered the window.
    PointerEnter {
        /// The window's surface.
        surface: u64,
    },
    /// The pointer left the window.
    PointerLeave {
        /// The window's surface.
        surface: u64,
    },
    /// A key changed.
    Key {
        /// Linux evdev keycode.
        keysym: u32,
        /// Pressed or released.
        pressed: bool,
        /// Diagnostic xkb-style modifier bits; the session tracks its own.
        modifiers: u32,
    },
    /// Keyboard focus moved to or away from the window.
    KeyboardFocus {
        /// The window's surface.
        surface: u64,
        /// Gained or lost.
        focused: bool,
    },
    /// The wheel or a trackpad scrolled, in surface pixels.
    PointerAxis {
        /// Horizontal distance.
        dx: f64,
        /// Vertical distance.
        dy: f64,
    },
    /// The viewer pasted text: the session offers it as the clipboard of
    /// the window's seat, so the window's own paste reads it.
    ClipboardText {
        /// At most [`MAX_CLIPBOARD_BYTES`].
        text: String,
    },
    /// The viewer closed the window: the session asks it to close, as a
    /// person's close of its tab does.
    Close,
    /// Text the viewer's input method committed (a composed character, a
    /// dictated phrase, an emoji): the session delivers it into the focused
    /// field the way an input method does, not as key presses. Appended
    /// after [`Self::Close`] because postcard encodes the variant index.
    Text {
        /// Non-empty, at most [`MAX_TEXT_BYTES`].
        text: String,
    },
}

impl InputEvent {
    /// This event for `surface`: the surface a viewer names is never trusted.
    #[must_use]
    pub fn for_surface(self, surface: u64) -> Self {
        match self {
            Self::PointerEnter { .. } => Self::PointerEnter { surface },
            Self::PointerLeave { .. } => Self::PointerLeave { surface },
            Self::KeyboardFocus { focused, .. } => Self::KeyboardFocus { surface, focused },
            other => other,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::PointerMotion { x, y } => x.is_finite() && y.is_finite(),
            Self::PointerAxis { dx, dy } => dx.is_finite() && dy.is_finite(),
            Self::ClipboardText { text } => text.len() <= MAX_CLIPBOARD_BYTES,
            Self::Text { text } => !text.is_empty() && text.len() <= MAX_TEXT_BYTES,
            _ => true,
        }
    }
}

/// What a member sends on [`INPUT_ROUTE`] (postcard). With no events it
/// only asks whether the member may drive the window: `204` yes, `403` no.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputRequest {
    /// The room's workspace.
    pub workspace_id: u64,
    /// The device publishing the stream (its live publisher).
    pub publisher: [u8; 32],
    /// The stream's surface ([`Stream::surface`], parsed).
    pub surface: u64,
    /// The events, in order.
    pub events: Vec<InputEvent>,
}

impl InputRequest {
    /// Whether every event and request bound holds.
    pub fn valid(&self) -> bool {
        self.events.len() <= MAX_INPUT_EVENTS && self.events.iter().all(InputEvent::valid)
    }
}

/// What a member sends on [`CODECS_ROUTE`] (postcard): the video codecs its
/// page decodes, as `SurfaceStreamStart` labels. Labels outside
/// [`VIDEO_CODECS`] are ignored, so a newer page may name more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecsRequest {
    /// The room's workspace.
    pub workspace_id: u64,
    /// The codecs the page decodes.
    pub decode: Vec<String>,
}

impl CodecsRequest {
    /// Whether the request stays within its bounds.
    pub fn valid(&self) -> bool {
        self.decode.len() <= MAX_CODEC_LABELS
            && self
                .decode
                .iter()
                .all(|label| label.len() <= MAX_CODEC_LABEL_BYTES)
    }

    /// The [`VIDEO_CODECS`] it names, in that order.
    pub fn video_codecs(&self) -> Vec<&'static str> {
        VIDEO_CODECS
            .into_iter()
            .filter(|codec| self.decode.iter().any(|label| label == codec))
            .collect()
    }
}

/// One legacy window stream, as the index and each group's start name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    /// The session's surface id, 16 lowercase hex digits.
    pub surface: String,
    /// Desktop app id of the window.
    pub app: String,
    /// `SurfaceStreamStart` codec label: one of [`VIDEO_CODECS`], or [`PAINT`].
    pub codec: String,
    /// Picture width the session announced, pixels.
    pub width: u32,
    /// Picture height the session announced, pixels.
    pub height: u32,
    /// Record id of this stream's track under [`APP`].
    pub record: String,
}

impl Stream {
    /// Whether metadata names the canonical record of a bridged codec with
    /// bounded dimensions.
    pub fn valid(&self) -> bool {
        self.surface.len() == 16
            && self
                .surface
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && !self.app.is_empty()
            && self.app.len() <= 128
            && bridged_codec(&self.codec)
            && self.width > 0
            && self.height > 0
            && self.width <= 8192
            && self.height <= 8192
            && self.record == format!("@tilo:legacy-window:{}", self.surface)
    }

    /// The stream of `surface`.
    pub fn new(surface: u64, app: &str, codec: &str, width: u32, height: u32) -> Self {
        Self {
            surface: format!("{surface:016x}"),
            app: app
                .chars()
                .scan(0_usize, |len, ch| {
                    *len = len.saturating_add(ch.len_utf8());
                    (*len <= 128).then_some(ch)
                })
                .collect(),
            codec: codec.chars().take(32).collect(),
            width,
            height,
            record: stream_record(surface).record,
        }
    }
}

/// The index record of a device's namespace.
pub fn index_record() -> LiveRecord {
    LiveRecord {
        app: APP.into(),
        record: INDEX_RECORD.into(),
    }
}

/// The record carrying `surface` in its device's namespace.
pub fn stream_record(surface: u64) -> LiveRecord {
    LiveRecord {
        app: APP.into(),
        record: format!("@tilo:legacy-window:{surface:016x}"),
    }
}

/// The surface a [`GRANT_PREFIX`] capability names and whether it grants
/// (`true`) or revokes it.
pub fn parse_grant(capability: &str) -> Option<(u64, bool)> {
    let rest = capability.strip_prefix(GRANT_PREFIX)?;
    let (surface, state) = rest.split_once(':')?;
    let granted = match state {
        "granted" => true,
        "revoked" => false,
        _ => return None,
    };
    Some((surface.parse().ok()?, granted))
}

/// One legacy-window frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The publisher's current streams.
    Index(Vec<Stream>),
    /// First frame of every group: the stream the group's pictures belong to.
    Start(Stream),
    /// Part of one encoded frame.
    Chunk {
        /// [`CHUNK_KEY`], [`CHUNK_FIRST`], [`CHUNK_LAST`].
        flags: u8,
        /// Encoded-frame sequence within the stream.
        seq: u32,
        /// Sender capture time, microseconds.
        ts_us: u64,
        /// Length of the whole encoded frame.
        total: u32,
        /// This chunk's bytes of the encoded frame.
        data: Vec<u8>,
    },
    /// The stream ended.
    End,
}

impl Frame {
    /// # Errors
    ///
    /// [`LiveWireError::Invalid`] when the frame exceeds one ordered frame.
    pub fn encode(&self) -> Result<Vec<u8>, LiveWireError> {
        let json = |_| LiveWireError::Codec;
        let mut out = vec![FRAME_TAG];
        match self {
            Self::Index(streams) => {
                if streams.len() > MAX_STREAMS || !streams.iter().all(Stream::valid) {
                    return Err(LiveWireError::Invalid);
                }
                out.push(KIND_INDEX);
                out.extend(serde_json::to_vec(streams).map_err(json)?);
            }
            Self::Start(stream) => {
                if !stream.valid() {
                    return Err(LiveWireError::Invalid);
                }
                out.push(KIND_START);
                out.extend(serde_json::to_vec(stream).map_err(json)?);
            }
            Self::Chunk {
                flags,
                seq,
                ts_us,
                total,
                data,
            } => {
                if data.is_empty()
                    || data.len() > MAX_CHUNK_DATA
                    || *flags & !7 != 0
                    || *total == 0
                    || *total as usize > MAX_ENCODED_FRAME_BYTES
                    || data.len() > *total as usize
                {
                    return Err(LiveWireError::Invalid);
                }
                out.push(KIND_CHUNK);
                out.push(*flags);
                out.extend(seq.to_be_bytes());
                out.extend(ts_us.to_be_bytes());
                out.extend(total.to_be_bytes());
                out.extend_from_slice(data);
            }
            Self::End => out.push(KIND_END),
        }
        if out.len() > MAX_OPS_FRAME_BYTES {
            return Err(LiveWireError::Invalid);
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// [`LiveWireError::Codec`] for anything that is not a well-formed frame.
    pub fn decode(bytes: &[u8]) -> Result<Self, LiveWireError> {
        if bytes.len() > MAX_OPS_FRAME_BYTES {
            return Err(LiveWireError::Codec);
        }
        let codec = |_| LiveWireError::Codec;
        match bytes {
            [FRAME_TAG, KIND_INDEX, rest @ ..] => {
                let streams: Vec<Stream> = serde_json::from_slice(rest).map_err(codec)?;
                if streams.len() > MAX_STREAMS || !streams.iter().all(Stream::valid) {
                    return Err(LiveWireError::Codec);
                }
                Ok(Self::Index(streams))
            }
            [FRAME_TAG, KIND_START, rest @ ..] => {
                let stream: Stream = serde_json::from_slice(rest).map_err(codec)?;
                if !stream.valid() {
                    return Err(LiveWireError::Codec);
                }
                Ok(Self::Start(stream))
            }
            [FRAME_TAG, KIND_CHUNK, flags, header @ ..] if header.len() > CHUNK_HEADER - 3 => {
                let (seq, rest) = header.split_at(4);
                let (ts, rest) = rest.split_at(8);
                let (total, data) = rest.split_at(4);
                let total = u32::from_be_bytes(total.try_into().map_err(|_| LiveWireError::Codec)?);
                if flags & !7 != 0
                    || total == 0
                    || total as usize > MAX_ENCODED_FRAME_BYTES
                    || data.len() > total as usize
                {
                    return Err(LiveWireError::Codec);
                }
                Ok(Self::Chunk {
                    flags: *flags,
                    seq: u32::from_be_bytes(seq.try_into().map_err(|_| LiveWireError::Codec)?),
                    ts_us: u64::from_be_bytes(ts.try_into().map_err(|_| LiveWireError::Codec)?),
                    total,
                    data: data.to_vec(),
                })
            }
            [FRAME_TAG, KIND_END] => Ok(Self::End),
            _ => Err(LiveWireError::Codec),
        }
    }
}

/// Split one encoded frame into its chunk frames, in order.
///
/// # Errors
///
/// [`LiveWireError::Invalid`] for an empty frame or one over
/// [`MAX_ENCODED_FRAME_BYTES`].
pub fn chunks(
    encoded: &[u8],
    key: bool,
    seq: u32,
    ts_us: u64,
) -> Result<Vec<Vec<u8>>, LiveWireError> {
    if encoded.is_empty() || encoded.len() > MAX_ENCODED_FRAME_BYTES {
        return Err(LiveWireError::Invalid);
    }
    let total = u32::try_from(encoded.len()).map_err(|_| LiveWireError::Invalid)?;
    let parts = encoded.chunks(MAX_CHUNK_DATA).collect::<Vec<_>>();
    let last = parts.len().saturating_sub(1);
    parts
        .into_iter()
        .enumerate()
        .map(|(index, data)| {
            let mut flags = if key { CHUNK_KEY } else { 0 };
            if index == 0 {
                flags |= CHUNK_FIRST;
            }
            if index == last {
                flags |= CHUNK_LAST;
            }
            Frame::Chunk {
                flags,
                seq,
                ts_us,
                total,
                data: data.to_vec(),
            }
            .encode()
        })
        .collect()
}

/// H.264 video, Annex B access units. Every browser decodes it, and a page
/// that never said what it decodes is assumed to decode only this.
pub const H264: &str = "h264";
/// VP9 video, one frame (or superframe) per encoded frame. Royalty-free and
/// always encodable in software, so a session without an H.264 encoder
/// still has a codec browsers decode.
pub const VP9: &str = "vp9";
/// AV1 video, one temporal unit of OBUs with size fields per encoded frame.
pub const AV1: &str = "av1";
/// The video codecs the host bridges to browser members.
pub const VIDEO_CODECS: [&str; 3] = [H264, VP9, AV1];
/// The paint stream (`tilo_paint_stream::wire::CODEC`): a page as a display
/// list the page draws itself. Each frame is one postcard `Payload`, whose
/// first byte is its variant; `0` is a whole frame a member can start from.
pub const PAINT: &str = "tilo-paint";

/// Whether the host re-publishes streams with this codec to browser members.
pub fn bridged_codec(codec: &str) -> bool {
    codec == PAINT || VIDEO_CODECS.contains(&codec)
}

/// Whether `frame` of a `codec` stream begins a group a member can start from.
pub fn keyframe(codec: &str, frame: &[u8]) -> bool {
    match codec {
        PAINT => frame.first() == Some(&0),
        VP9 => vp9_keyframe(frame),
        AV1 => av1_keyframe(frame),
        _ => h264_keyframe(frame),
    }
}

/// Whether a VP9 frame is a key frame: its uncompressed header has the frame
/// marker `0b10`, does not repeat an earlier frame (`show_existing_frame`)
/// and has `frame_type` 0. A superframe's first frame decides.
pub fn vp9_keyframe(frame: &[u8]) -> bool {
    let Some(&first) = frame.first() else {
        return false;
    };
    if first >> 6 != 0b10 {
        return false;
    }
    // Both profile bits set is profile 3, which has a reserved zero bit
    // before `show_existing_frame`.
    let (show_existing, frame_type) = if first & 0b0011_0000 == 0b0011_0000 {
        (0b0100, 0b0010)
    } else {
        (0b1000, 0b0100)
    };
    first & (show_existing | frame_type) == 0
}

/// Whether an AV1 temporal unit (OBUs with size fields) carries a sequence
/// header: encoders send one with every key frame, and a decoder cannot
/// start without one.
pub fn av1_keyframe(temporal_unit: &[u8]) -> bool {
    const OBU_SEQUENCE_HEADER: u8 = 1;
    let mut at = 0_usize;
    while let Some(&header) = temporal_unit.get(at) {
        if (header >> 3) & 0x0f == OBU_SEQUENCE_HEADER {
            return true;
        }
        if (header >> 1) & 1 == 0 {
            // Without a size field the rest of the unit is this one OBU.
            return false;
        }
        let Some(size_at) = at
            .checked_add(1)
            .and_then(|after| after.checked_add(usize::from((header >> 2) & 1)))
        else {
            return false;
        };
        let Some((size, used)) = temporal_unit.get(size_at..).and_then(leb128) else {
            return false;
        };
        let Some(next) = size_at
            .checked_add(used)
            .and_then(|start| start.checked_add(size))
        else {
            return false;
        };
        at = next;
    }
    false
}

/// An unsigned LEB128 value of at most eight bytes, and the bytes it took.
fn leb128(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().take(8).enumerate() {
        let shift = u32::try_from(index).ok()?.checked_mul(7)?;
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((usize::try_from(value).ok()?, index.checked_add(1)?));
        }
    }
    None
}

/// Whether an H.264 Annex B access unit holds an IDR slice.
pub fn h264_keyframe(access_unit: &[u8]) -> bool {
    access_unit
        .windows(4)
        .any(|window| matches!(window, [0, 0, 1, header] if header & 0x1f == 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::LiveFrame;

    fn stream() -> Stream {
        Stream::new(7, "org.chromium.Chromium", "h264", 1280, 720)
    }

    #[test]
    fn malformed_metadata_and_chunks_are_refused() {
        let mut bad = stream();
        bad.record = "@tilo:private-document".into();
        assert!(Frame::Start(bad).encode().is_err());
        let mut bad = stream();
        bad.width = 0;
        assert!(Frame::Index(vec![bad]).encode().is_err());
        let over_limit = u32::try_from(MAX_ENCODED_FRAME_BYTES.checked_add(1).unwrap()).unwrap();
        for (flags, total) in [(8, 1), (7, 0), (7, over_limit)] {
            assert!(
                Frame::Chunk {
                    flags,
                    seq: 0,
                    ts_us: 0,
                    total,
                    data: vec![1]
                }
                .encode()
                .is_err()
            );
        }
    }

    #[test]
    fn frames_round_trip_and_never_decode_as_live_frames() {
        let frames = [
            Frame::Index(vec![stream()]),
            Frame::Index(Vec::new()),
            Frame::Start(stream()),
            Frame::Chunk {
                flags: CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST,
                seq: 3,
                ts_us: 99,
                total: 3,
                data: vec![0, 0, 1],
            },
            Frame::End,
        ];
        for frame in frames {
            let bytes = frame.encode().unwrap();
            assert_eq!(Frame::decode(&bytes).unwrap(), frame);
            assert!(
                LiveFrame::decode(&bytes).is_err(),
                "a member could publish {frame:?} on a legacy record"
            );
        }
        assert!(Frame::decode(&[FRAME_TAG, KIND_CHUNK, 0, 0, 0]).is_err());
    }

    /// The browser decoder (`web/legacy-windows.test.mjs`) pins the same bytes.
    #[test]
    fn a_chunk_encodes_to_its_pinned_bytes() {
        let chunk = Frame::Chunk {
            flags: CHUNK_KEY | CHUNK_FIRST | CHUNK_LAST,
            seq: 9,
            ts_us: 99,
            total: 3,
            data: vec![0, 0, 1],
        };
        assert_eq!(
            hex::encode(chunk.encode().unwrap()),
            "f1030700000009000000000000006300000003000001"
        );
    }

    #[test]
    fn a_large_frame_is_split_into_ordered_chunks_that_reassemble() {
        let encoded = (0..=u8::MAX)
            .cycle()
            .take(MAX_CHUNK_DATA * 2 + 5)
            .collect::<Vec<_>>();
        let parts = chunks(&encoded, true, 9, 1).unwrap();
        assert_eq!(parts.len(), 3);
        let mut joined = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            assert!(part.len() <= MAX_OPS_FRAME_BYTES);
            let Frame::Chunk {
                flags,
                seq,
                total,
                data,
                ..
            } = Frame::decode(part).unwrap()
            else {
                panic!("not a chunk");
            };
            assert_eq!((seq, total as usize), (9, encoded.len()));
            assert_eq!(flags & CHUNK_KEY, CHUNK_KEY);
            assert_eq!(flags & CHUNK_FIRST != 0, index == 0);
            assert_eq!(flags & CHUNK_LAST != 0, index == 2);
            joined.extend(data);
        }
        assert_eq!(joined, encoded);
        assert!(chunks(&[], false, 0, 0).is_err());
        assert!(chunks(&vec![1; MAX_ENCODED_FRAME_BYTES + 1], false, 0, 0).is_err());
    }

    #[test]
    fn stream_records_are_distinct_and_named_by_the_index() {
        let a = stream_record(7);
        assert_ne!(a.digest(), stream_record(8).digest());
        assert_ne!(a.digest(), index_record().digest());
        let listed = Stream::new(7, "app", "h264", 2, 2);
        assert_eq!(listed.record, a.record);
        assert_eq!(listed.surface, "0000000000000007");
    }

    #[test]
    fn grants_name_their_surface() {
        assert_eq!(parse_grant("workspace-control:7:granted"), Some((7, true)));
        assert_eq!(parse_grant("workspace-control:7:revoked"), Some((7, false)));
        assert_eq!(parse_grant("workspace-control:x:granted"), None);
        assert_eq!(parse_grant("workspace-control:7:maybe"), None);
        assert_eq!(parse_grant("share-screen"), None);
    }

    /// The session decodes `RemoteInput::event` as its own `InputEvent`; the
    /// variants it has share these encodings (`shell::network::streaming`).
    #[test]
    fn input_events_keep_the_sessions_encoding() {
        let cases: [(InputEvent, &str); 6] = [
            (
                InputEvent::PointerMotion { x: 1.5, y: 2.0 },
                "00000000000000f83f0000000000000040",
            ),
            (
                InputEvent::PointerButton {
                    button: 272,
                    pressed: true,
                },
                "01900201",
            ),
            (
                InputEvent::Key {
                    keysym: 30,
                    pressed: false,
                    modifiers: 0,
                },
                "041e0000",
            ),
            (
                InputEvent::KeyboardFocus {
                    surface: 7,
                    focused: true,
                },
                "050701",
            ),
            (InputEvent::Close, "08"),
            // The session's `Text` is its tenth variant, after `Close`.
            (
                InputEvent::Text {
                    text: "日本".into(),
                },
                "0906e697a5e69cac",
            ),
        ];
        for (event, bytes) in cases {
            assert_eq!(hex::encode(postcard::to_allocvec(&event).unwrap()), bytes);
        }
    }

    #[test]
    fn input_requests_are_bounded_and_the_named_surface_is_replaced() {
        let request = |events| InputRequest {
            workspace_id: 1,
            publisher: [2; 32],
            surface: 7,
            events,
        };
        assert!(request(Vec::new()).valid());
        assert!(request(vec![InputEvent::PointerAxis { dx: 0.0, dy: 3.0 }]).valid());
        assert!(
            !request(vec![InputEvent::PointerMotion {
                x: f64::NAN,
                y: 0.0
            }])
            .valid()
        );
        assert!(
            !request(vec![InputEvent::ClipboardText {
                text: "x".repeat(MAX_CLIPBOARD_BYTES + 1)
            }])
            .valid()
        );
        assert!(
            request(vec![InputEvent::Text {
                text: "é".repeat(MAX_TEXT_BYTES / 2)
            }])
            .valid()
        );
        for text in [String::new(), "x".repeat(MAX_TEXT_BYTES + 1)] {
            assert!(!request(vec![InputEvent::Text { text }]).valid());
        }
        assert!(
            !request(vec![
                InputEvent::PointerLeave { surface: 1 };
                MAX_INPUT_EVENTS + 1
            ])
            .valid()
        );
        assert_eq!(
            InputEvent::PointerEnter { surface: 99 }.for_surface(7),
            InputEvent::PointerEnter { surface: 7 }
        );
    }

    #[test]
    fn paint_streams_are_bridged_and_key_on_whole_frames() {
        assert!(Stream::new(7, "so.tilo.browser", PAINT, 1280, 720).valid());
        assert!(!Stream::new(7, "so.tilo.browser", "hevc-main10", 1280, 720).valid());
        assert!(keyframe(PAINT, &[0, 1, 2]));
        assert!(!keyframe(PAINT, &[1, 0]));
        assert!(!keyframe(PAINT, &[]));
        // An IDR-looking paint payload is not a keyframe, nor the reverse.
        assert!(!keyframe(PAINT, &[2, 0, 0, 1, 0x65]));
        assert!(keyframe(H264, &[0, 0, 1, 0x65]));
    }

    /// The first bytes of frames libvpx (profile 0, realtime) and SVT-AV1
    /// (low delay) encoded from a test pattern: a key frame and the inter
    /// frame after it.
    const VP9_KEY: [u8; 16] = [
        0x82, 0x49, 0x83, 0x42, 0x00, 0x00, 0xf0, 0x00, 0xf6, 0x06, 0x38, 0x24, 0x1c, 0x18, 0x42,
        0x00,
    ];
    const VP9_INTER: [u8; 17] = [
        0x86, 0x00, 0x40, 0x92, 0x9c, 0x28, 0x49, 0x40, 0x00, 0x03, 0x70, 0x00, 0x00, 0x09, 0xd0,
        0x28, 0x00,
    ];
    const AV1_KEY: [u8; 20] = [
        0x12, 0x00, 0x0a, 0x0b, 0x02, 0x00, 0x00, 0x04, 0x95, 0x7f, 0xfc, 0x4a, 0xf9, 0x00, 0x40,
        0x32, 0xfc, 0x09, 0x10, 0x00,
    ];
    const AV1_INTER: [u8; 20] = [
        0x12, 0x00, 0x32, 0x10, 0x30, 0x02, 0x00, 0x00, 0x00, 0x00, 0x23, 0x18, 0x00, 0x00, 0x04,
        0x7c, 0x02, 0x00, 0x92, 0xd0,
    ];

    #[test]
    fn vp9_and_av1_streams_are_bridged_and_key_on_their_key_frames() {
        for codec in [VP9, AV1] {
            assert!(bridged_codec(codec));
            assert!(Stream::new(7, "org.chromium.Chromium", codec, 1280, 720).valid());
        }
        assert!(keyframe(VP9, &VP9_KEY));
        assert!(!keyframe(VP9, &VP9_INTER));
        assert!(keyframe(AV1, &AV1_KEY));
        assert!(!keyframe(AV1, &AV1_INTER));
        // Each codec reads only its own header.
        assert!(!keyframe(H264, &VP9_KEY));
        assert!(!keyframe(VP9, &[0, 0, 1, 0x65]));
        assert!(!keyframe(AV1, &[0, 0, 1, 0x65]));
        assert!(!keyframe(VP9, &[]));
        assert!(!keyframe(AV1, &[]));
        // A repeated frame is no key frame; profile 3's reserved bit shifts
        // the flags.
        assert!(!vp9_keyframe(&[0b1000_1000]));
        assert!(vp9_keyframe(&[0b1011_0000]));
        assert!(!vp9_keyframe(&[0b1011_0100]));
        assert!(!vp9_keyframe(&[0b1011_0010]));
        // An AV1 unit whose sizes run past its end is not one.
        assert!(!av1_keyframe(&[0x12, 0xff, 0xff]));
        assert!(!av1_keyframe(&[0x12, 0x05, 0x0a]));
    }

    #[test]
    fn codec_requests_are_bounded_and_name_only_bridged_video() {
        let request = |decode: &[&str]| CodecsRequest {
            workspace_id: 1,
            decode: decode.iter().map(|label| (*label).to_owned()).collect(),
        };
        assert_eq!(
            request(&["av1", "hevc", "h264", "vp9", "tilo-paint"]).video_codecs(),
            [H264, VP9, AV1]
        );
        assert!(request(&[]).video_codecs().is_empty());
        assert!(request(&["h264"; MAX_CODEC_LABELS]).valid());
        assert!(!request(&["h264"; MAX_CODEC_LABELS + 1]).valid());
        assert!(!request(&[&"x".repeat(MAX_CODEC_LABEL_BYTES + 1)]).valid());
    }

    #[test]
    fn idr_access_units_are_keyframes() {
        let idr = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0, 0, 1, 0x65, 0x88];
        let delta = [0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x01];
        assert!(h264_keyframe(&idr));
        assert!(h264_keyframe(&[0, 0, 1, 0x25, 0x88]));
        assert!(!h264_keyframe(&delta));
        assert!(!h264_keyframe(&[]));
    }
}
