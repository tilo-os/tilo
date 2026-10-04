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

/// One legacy window stream, as the index and each group's start name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    /// The session's surface id, 16 lowercase hex digits.
    pub surface: String,
    /// Desktop app id of the window.
    pub app: String,
    /// `SurfaceStreamStart` codec label (`"h264"`).
    pub codec: String,
    /// Picture width the session announced, pixels.
    pub width: u32,
    /// Picture height the session announced, pixels.
    pub height: u32,
    /// Record id of this stream's track under [`APP`].
    pub record: String,
}

impl Stream {
    /// Whether metadata names the canonical H.264 record with bounded dimensions.
    pub fn valid(&self) -> bool {
        self.surface.len() == 16
            && self
                .surface
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && !self.app.is_empty()
            && self.app.len() <= 128
            && self.codec == "h264"
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
        let cases: [(InputEvent, &str); 4] = [
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
    fn idr_access_units_are_keyframes() {
        let idr = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0, 0, 1, 0x65, 0x88];
        let delta = [0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x01];
        assert!(h264_keyframe(&idr));
        assert!(h264_keyframe(&[0, 0, 1, 0x25, 0x88]));
        assert!(!h264_keyframe(&delta));
        assert!(!h264_keyframe(&[]));
    }
}
