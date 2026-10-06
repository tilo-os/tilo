//! Legacy (non-Tilo) windows the session streams as video, carried to browser
//! viewers on `LIVE_V1` ordered record tracks.
//!
//! Only the host writes these tracks: it injects them into the namespace of the
//! session device that published the `SurfaceStream*` messages, and no client
//! can: the relay admits a client's ordered frame only when it decodes as an
//! [`OpsFrame`](super::OpsFrame), and every frame here starts with
//! [`FRAME_TAG`], which never begins a valid one.
//!
//! * The index record ([`index_record`]) carries one [`Frame::Index`] listing
//!   the session's current streams, re-sent while any stream is live.
//! * Each stream has its own record ([`stream_record`]). One group per coded
//!   video sequence: [`Frame::Start`], then the keyframe's [`Frame::Chunk`]s,
//!   then later frames' chunks, so a subscriber that starts at the latest group
//!   can decode at once. An encoded frame larger than one ordered frame is
//!   split into consecutive chunks of that group. [`Frame::End`] closes it.
use super::{LiveRecord, LiveWireError, MAX_STREAM_FRAME_BYTES};
use serde::{Deserialize, Serialize};

/// Application id of every legacy-window record.
pub const APP: &str = "so.tilo.legacy-window";
/// Record id of a session's stream index.
pub const INDEX_RECORD: &str = "@tilo:legacy-windows";
/// First byte of every frame; `0xF1` is never the first byte of an `OpsFrame`.
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
pub const MAX_CHUNK_DATA: usize = MAX_STREAM_FRAME_BYTES - CHUNK_HEADER;
/// Largest encoded frame the host forwards; larger ones are dropped.
pub const MAX_ENCODED_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Streams one index may list.
pub const MAX_STREAMS: usize = 32;
/// Proof-required HTTPS route a controlling viewer sends input on.
pub const INPUT_TARGET: &str = "/peer/v3/live/legacy-input";
/// Input request body ceiling.
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
/// Events one input request may carry.
pub const MAX_INPUT_EVENTS: usize = 64;

/// One input event for a legacy window: the session's remote-input
/// vocabulary (`RemoteInput::event`), variant for variant, so its postcard
/// encoding is the one the session decodes. Coordinates are surface-local.
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
        #[serde(default)]
        modifiers: u32,
    },
    /// Keyboard focus moved to or away from the window.
    KeyboardFocus {
        /// The window's surface.
        surface: u64,
        /// Gained or lost.
        focused: bool,
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
}

/// What a controlling viewer sends on [`INPUT_TARGET`] (JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRequest {
    /// The stream's record id ([`Stream::record`]).
    pub record: String,
    /// The events, in order.
    pub events: Vec<InputEvent>,
}
impl InputRequest {
    /// # Errors
    ///
    /// [`LiveWireError::Codec`] for an oversized or malformed body,
    /// [`LiveWireError::Invalid`] for no, too many or non-finite events.
    pub fn decode(bytes: &[u8]) -> Result<Self, LiveWireError> {
        if bytes.len() > MAX_INPUT_BYTES {
            return Err(LiveWireError::Codec);
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|_| LiveWireError::Codec)?;
        let finite = request.events.iter().all(|event| match event {
            InputEvent::PointerMotion { x, y } => x.is_finite() && y.is_finite(),
            _ => true,
        });
        if request.events.is_empty() || request.events.len() > MAX_INPUT_EVENTS || !finite {
            return Err(LiveWireError::Invalid);
        }
        Ok(request)
    }
}

/// One legacy window stream, as the index and each group's start name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    /// Publishing session's peer handle, 16 lowercase hex digits.
    pub source: String,
    /// The session's surface id, 16 lowercase hex digits.
    pub surface: String,
    /// Desktop app id of the window.
    pub app: String,
    /// `SurfaceStreamStart` codec label (`"h264"`, `"vp9"`, `"av1"`).
    pub codec: String,
    /// Picture width the session announced, pixels.
    pub width: u32,
    /// Picture height the session announced, pixels.
    pub height: u32,
    /// Record id of this stream's track under [`APP`].
    pub record: String,
}
impl Stream {
    /// The stream of `surface` published by the session `source`.
    pub fn new(source: u64, surface: u64, app: &str, codec: &str, width: u32, height: u32) -> Self {
        Self {
            source: format!("{source:016x}"),
            surface: format!("{surface:016x}"),
            app: app.chars().take(128).collect(),
            codec: codec.chars().take(32).collect(),
            width,
            height,
            record: stream_record(source, surface).record,
        }
    }
}

/// The index record of a session's namespace.
pub fn index_record() -> LiveRecord {
    LiveRecord {
        app: APP.into(),
        record: INDEX_RECORD.into(),
    }
}

/// The record carrying `surface` of the session `source`.
pub fn stream_record(source: u64, surface: u64) -> LiveRecord {
    LiveRecord {
        app: APP.into(),
        record: format!("@tilo:legacy-window:{source:016x}:{surface:016x}"),
    }
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
                if streams.len() > MAX_STREAMS {
                    return Err(LiveWireError::Invalid);
                }
                out.push(KIND_INDEX);
                out.extend(serde_json::to_vec(streams).map_err(json)?);
            }
            Self::Start(stream) => {
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
                if data.is_empty() || data.len() > MAX_CHUNK_DATA {
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
        if out.len() > MAX_STREAM_FRAME_BYTES {
            return Err(LiveWireError::Invalid);
        }
        Ok(out)
    }

    /// # Errors
    ///
    /// [`LiveWireError::Codec`] for anything that is not a well-formed frame.
    pub fn decode(bytes: &[u8]) -> Result<Self, LiveWireError> {
        if bytes.len() > MAX_STREAM_FRAME_BYTES {
            return Err(LiveWireError::Codec);
        }
        let codec = |_| LiveWireError::Codec;
        match bytes {
            [FRAME_TAG, KIND_INDEX, rest @ ..] => {
                let streams: Vec<Stream> = serde_json::from_slice(rest).map_err(codec)?;
                if streams.len() > MAX_STREAMS {
                    return Err(LiveWireError::Codec);
                }
                Ok(Self::Index(streams))
            }
            [FRAME_TAG, KIND_START, rest @ ..] => {
                Ok(Self::Start(serde_json::from_slice(rest).map_err(codec)?))
            }
            [
                FRAME_TAG,
                KIND_CHUNK,
                flags,
                s0,
                s1,
                s2,
                s3,
                t0,
                t1,
                t2,
                t3,
                t4,
                t5,
                t6,
                t7,
                n0,
                n1,
                n2,
                n3,
                data @ ..,
            ] if !data.is_empty() => Ok(Self::Chunk {
                flags: *flags,
                seq: u32::from_be_bytes([*s0, *s1, *s2, *s3]),
                ts_us: u64::from_be_bytes([*t0, *t1, *t2, *t3, *t4, *t5, *t6, *t7]),
                total: u32::from_be_bytes([*n0, *n1, *n2, *n3]),
                data: data.to_vec(),
            }),
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
    use crate::delivery_v3::live_wire::OpsFrame;

    fn stream() -> Stream {
        Stream::new(0xABCD, 7, "org.chromium.Chromium", "h264", 1280, 720)
    }

    #[test]
    fn frames_round_trip_and_never_decode_as_ops_frames() {
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
                OpsFrame::decode(&bytes).is_err(),
                "a client could publish {frame:?} on a legacy record"
            );
        }
    }

    /// The frame layout is fixed: these bytes are the wire contract.
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
            assert!(part.len() <= MAX_STREAM_FRAME_BYTES);
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
        let a = stream_record(1, 7);
        assert_ne!(a.digest(), stream_record(2, 7).digest());
        assert_ne!(a.digest(), stream_record(1, 8).digest());
        assert_ne!(a.digest(), index_record().digest());
        let listed = Stream::new(1, 7, "app", "h264", 2, 2);
        assert_eq!(listed.record, a.record);
        assert_eq!(
            (listed.source.as_str(), listed.surface.as_str()),
            ("0000000000000001", "0000000000000007")
        );
    }

    #[test]
    fn input_requests_are_bounded_and_the_named_surface_is_replaced() {
        let body = br#"{"record":"r","events":[{"PointerMotion":{"x":1.5,"y":2}},{"PointerEnter":{"surface":99}},{"Key":{"keysym":30,"pressed":true}}]}"#;
        let request = InputRequest::decode(body).unwrap();
        assert_eq!(request.events.len(), 3);
        assert_eq!(
            request.events[1].clone().for_surface(7),
            InputEvent::PointerEnter { surface: 7 }
        );
        assert_eq!(
            request.events[2],
            InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0
            }
        );
        assert!(InputRequest::decode(br#"{"record":"r","events":[]}"#).is_err());
        assert!(InputRequest::decode(br#"{"record":"r","events":[],"x":1}"#).is_err());
        assert!(InputRequest::decode(&vec![b' '; MAX_INPUT_BYTES + 1]).is_err());
    }

    #[test]
    fn idr_access_units_are_keyframes() {
        let idr = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0, 0, 1, 0x65, 0x88];
        let delta = [0, 0, 0, 1, 0x41, 0x9a, 0, 0, 1, 0x01];
        assert!(h264_keyframe(&idr));
        assert!(
            h264_keyframe(&[0, 0, 1, 0x25, 0x88]),
            "three-byte start code, other nal_ref_idc"
        );
        assert!(!h264_keyframe(&delta));
        assert!(!h264_keyframe(&[]));
    }
}
