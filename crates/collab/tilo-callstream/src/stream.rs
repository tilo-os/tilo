//! Per-share frame-transport substrate for encoded video frames.
//!
//! Video wants drop-oldest, not reliable retransmit: a dropped frame isn't
//! worth re-delivering since the next frame is already better data. Each
//! subscriber gets an independent bounded queue with drop-oldest
//! backpressure and a drop counter; a sequence-gap detector on the receive
//! side tells a late-joining viewer to wait for the next keyframe.
//!
//! Each frame carries a fixed 13-byte header, then the encoded access unit:
//!
//! ```text
//! offset  bytes  field
//! 0       8      sequence   u64 big-endian, per-stream monotonic
//! 8       4      length     u32 big-endian, byte count of everything after flags
//! 12      1      flags      bit 0   keyframe
//!                           bits 1-3 codec: 0 H.264, 1 VP9, 2 AV1, 3 HEVC Main10
//!                           bit 4   display size present
//!                           bits 5-7 reserved (must be zero)
//! 13      4      display    u16 BE width, u16 BE height (only with bit 4)
//! ..      N      payload    the access unit
//! ```
//!
//! An H.264 frame is byte-identical to the original format (codec 0, no
//! display size), so peers that predate the codec field keep decoding H.264
//! streams. Those peers reject any other codec as reserved flag bits rather
//! than mis-decoding it. Frames of the other codecs always carry the display
//! size: some hardware encoders pad the coded picture without signalling the
//! size to show (see `media::DecoderConfig::display_size`).
//!
//! The byte transport itself — compositor gossip/QUIC, or the call
//! app's iroh-QUIC media transport — lives outside this module, keeping
//! framing and backpressure unit-testable without a running network stack.

use media::StreamCodec;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_lock::MutexRecover as _;

use encoded_frame::{
    Codec as FrameCodec, FrameMeta, MAX_CALLSTREAM_PAYLOAD, Profile, Sequence, SequenceError,
    SequenceEvent, validate_payload,
};

/// Opaque numeric peer handle; matches the compositor's `network::protocol::PeerId`.
pub type PeerId = u64;

/// Opaque numeric handle for stream routing; matches the compositor's `network::protocol::SurfaceId`.
pub type SurfaceId = u64;

/// Fixed header size in bytes: u64 sequence + u32 length + u8 flags.
pub const FRAME_HEADER_LEN: usize = 8 + 4 + 1;

/// Largest accepted encoded access unit.  This is deliberately far below the
/// `u32` wire representation: a single video frame larger than 8 MiB is not
/// useful to the realtime path, and accepting it would let a peer reserve an
/// unbounded amount of receiver memory.
pub const MAX_ENCODED_FRAME_LEN: usize = MAX_CALLSTREAM_PAYLOAD;

/// Largest amount of incomplete wire data retained for one stream.
///
/// A complete maximum-sized frame fits, but a peer cannot retain arbitrary
/// bytes by declaring a frame and never finishing it.
pub const MAX_BUFFERED_STREAM_LEN: usize =
    FRAME_HEADER_LEN + DISPLAY_SIZE_LEN + MAX_ENCODED_FRAME_LEN;

/// Bit 0 of the header's `flags` byte: this unit is a keyframe.
pub const FLAG_KEYFRAME: u8 = 0b0000_0001;
/// Bits 1-3 of the header's `flags` byte: the codec.
pub const FLAG_CODEC_MASK: u8 = 0b0000_1110;
/// Bit 4 of the header's `flags` byte: a 4-byte display size precedes the payload.
pub const FLAG_DISPLAY_SIZE: u8 = 0b0001_0000;
/// Bytes of the optional display-size field.
pub const DISPLAY_SIZE_LEN: usize = 4;

/// The codec's 3-bit id in the flags byte.
fn codec_id(codec: StreamCodec) -> Option<u8> {
    Profile::Callstream.codec_id(shared_codec(codec))
}

fn codec_from_id(id: u8) -> Option<StreamCodec> {
    Profile::Callstream.codec(id).and_then(media_codec)
}

/// The shared contract's codec for a media-stack codec.
const fn shared_codec(codec: StreamCodec) -> FrameCodec {
    match codec {
        StreamCodec::H264 => FrameCodec::H264,
        StreamCodec::Vp9 => FrameCodec::Vp9,
        StreamCodec::Av1 => FrameCodec::Av1,
        StreamCodec::HevcMain10 => FrameCodec::HevcMain10,
    }
}

/// The media-stack codec for a shared contract codec the profile carries.
const fn media_codec(codec: FrameCodec) -> Option<StreamCodec> {
    match codec {
        FrameCodec::H264 => Some(StreamCodec::H264),
        FrameCodec::HevcMain10 => Some(StreamCodec::HevcMain10),
        FrameCodec::Vp9 => Some(StreamCodec::Vp9),
        FrameCodec::Av1 => Some(StreamCodec::Av1),
        FrameCodec::Vp8 => None,
    }
}

/// Bounded frame-queue size per subscriber. 3 frames matches a typical
/// paint side's tolerance before drop-oldest kicks in.
pub const DEFAULT_QUEUE_CAP: usize = 3;

/// A single frame flowing across the substream. The payload is packed and
/// consumed without inspection.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceFrame {
    /// Per-stream sequence number; starts at 0 and increments once per `publish`.
    pub sequence: u64,
    /// Whether this is a keyframe. Late-joining viewers must start at a
    /// keyframe or the decoder emits garbage until the next one.
    pub keyframe: bool,
    /// The bitstream the payload is in.
    pub codec: StreamCodec,
    /// The picture size to show. Written for every codec but H.264 (whose
    /// frames stay in the original format); `None` on H.264 frames.
    pub display_size: Option<(u16, u16)>,
    pub payload: Vec<u8>,
}

impl SurfaceFrame {
    /// An H.264 frame in the original format.
    pub fn h264(sequence: u64, keyframe: bool, payload: Vec<u8>) -> Self {
        Self {
            sequence,
            keyframe,
            codec: StreamCodec::H264,
            display_size: None,
            payload,
        }
    }
}

impl SurfaceFrame {
    /// The shared contract's view of this picture.
    pub fn meta(&self) -> FrameMeta {
        let meta = FrameMeta::new(self.keyframe, shared_codec(self.codec), self.sequence);
        if self.codec == StreamCodec::H264 {
            meta
        } else {
            let (width, height) = self.display_size.unwrap_or((0, 0));
            meta.with_display_size(width, height)
        }
    }

    /// Serialises into the wire format above (one allocation).
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::PayloadTooLarge`] instead of emitting an invalid
    /// length prefix when the payload exceeds [`MAX_ENCODED_FRAME_LEN`].
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        validate_payload(self.payload.len(), MAX_ENCODED_FRAME_LEN)
            .map_err(|_| FrameError::PayloadTooLarge(self.payload.len()))?;
        // H.264 frames stay byte-identical to the original format.
        let display = match self.codec {
            StreamCodec::H264 => None,
            _ => Some(self.display_size.unwrap_or((0, 0))),
        };
        let body_len = if display.is_some() {
            self.payload.len().checked_add(DISPLAY_SIZE_LEN)
        } else {
            Some(self.payload.len())
        }
        .ok_or(FrameError::LengthOverflow(self.payload.len()))?;
        let capacity = FRAME_HEADER_LEN
            .checked_add(body_len)
            .ok_or(FrameError::LengthOverflow(self.payload.len()))?;
        let mut buf = Vec::with_capacity(capacity);
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        let len: u32 = body_len
            .try_into()
            .map_err(|_| FrameError::PayloadTooLarge(self.payload.len()))?;
        buf.extend_from_slice(&len.to_be_bytes());
        let id = codec_id(self.codec).ok_or(FrameError::UnsupportedCodec(self.codec))?;
        let mut flags = id << 1;
        if self.keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if display.is_some() {
            flags |= FLAG_DISPLAY_SIZE;
        }
        buf.push(flags);
        if let Some((w, h)) = display {
            buf.extend_from_slice(&w.to_be_bytes());
            buf.extend_from_slice(&h.to_be_bytes());
        }
        buf.extend_from_slice(&self.payload);
        Ok(buf)
    }

    /// Parses one frame off the front of `input`. Returns the frame and bytes
    /// consumed, or `None` if `input` doesn't yet hold a full frame.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::PayloadTooLarge`] for a length larger than the
    /// realtime frame limit, and [`FrameError::InvalidFlags`] for reserved
    /// flag bits.
    pub fn decode(input: &[u8]) -> Result<Option<(SurfaceFrame, usize)>, FrameError> {
        if input.len() < FRAME_HEADER_LEN {
            return Ok(None);
        }
        // Every slice below is inside the just-checked `FRAME_HEADER_LEN`
        // prefix (or, for the payload, the `total <= input.len()` checked
        // further down) — `input` is untrusted network data, so each access
        // still goes through `.get()` rather than trusting that bound.
        let Some(sequence_bytes) = input.get(0..8).and_then(|s| <[u8; 8]>::try_from(s).ok()) else {
            return Ok(None);
        };
        let sequence = u64::from_be_bytes(sequence_bytes);

        let Some(length_bytes) = input.get(8..12).and_then(|s| <[u8; 4]>::try_from(s).ok()) else {
            return Ok(None);
        };
        let length = u32::from_be_bytes(length_bytes) as usize;

        let Some(&flags) = input.get(12) else {
            return Ok(None);
        };

        let has_display = flags & FLAG_DISPLAY_SIZE != 0;
        let limit = if has_display {
            MAX_ENCODED_FRAME_LEN.saturating_add(DISPLAY_SIZE_LEN)
        } else {
            MAX_ENCODED_FRAME_LEN
        };
        validate_payload(length, limit).map_err(|_| FrameError::PayloadTooLarge(length))?;
        if flags & !(FLAG_KEYFRAME | FLAG_CODEC_MASK | FLAG_DISPLAY_SIZE) != 0 {
            return Err(FrameError::InvalidFlags(flags));
        }
        let codec =
            codec_from_id((flags & FLAG_CODEC_MASK) >> 1).ok_or(FrameError::InvalidFlags(flags))?;
        if has_display && length < DISPLAY_SIZE_LEN {
            return Err(FrameError::InvalidFlags(flags));
        }
        let total = FRAME_HEADER_LEN
            .checked_add(length)
            .ok_or(FrameError::LengthOverflow(length))?;
        if input.len() < total {
            return Ok(None);
        }
        let Some(body) = input.get(FRAME_HEADER_LEN..total) else {
            return Ok(None);
        };
        let (display_size, payload_slice) = if has_display {
            let (size, rest) = body
                .split_at_checked(DISPLAY_SIZE_LEN)
                .ok_or(FrameError::InvalidFlags(flags))?;
            let &[w0, w1, h0, h1] = size else {
                return Err(FrameError::InvalidFlags(flags));
            };
            (
                Some((u16::from_be_bytes([w0, w1]), u16::from_be_bytes([h0, h1]))),
                rest,
            )
        } else {
            (None, body)
        };
        validate_payload(payload_slice.len(), MAX_ENCODED_FRAME_LEN)
            .map_err(|_| FrameError::PayloadTooLarge(payload_slice.len()))?;
        let payload = payload_slice.to_vec();
        Ok(Some((
            SurfaceFrame {
                sequence,
                keyframe: flags & FLAG_KEYFRAME != 0,
                codec,
                display_size,
                payload,
            },
            total,
        )))
    }
}

/// Framing errors surfaced by [`SurfaceFrame::decode`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FrameError {
    /// The length prefix cannot be represented safely on this platform.
    #[error("frame length {0} overflowed u32")]
    LengthOverflow(usize),
    /// A peer or local producer attempted to send a frame larger than the
    /// realtime frame budget.
    #[error("frame payload {0} exceeds the {MAX_ENCODED_FRAME_LEN}-byte limit")]
    PayloadTooLarge(usize),
    /// A peer attempted to retain too much incomplete stream data.
    #[error("buffered stream data {0} exceeds the {MAX_BUFFERED_STREAM_LEN}-byte limit")]
    BufferedStreamTooLarge(usize),
    /// The frame uses currently-unassigned flag bits.
    #[error("frame uses reserved flag bits: {0:#010b}")]
    InvalidFlags(u8),
    /// The frame's codec has no id in this profile.
    #[error("codec {0:?} is not carried by the callstream profile")]
    UnsupportedCodec(StreamCodec),
    /// The finite sequence space has been exhausted; callers must establish a
    /// fresh stream instead of wrapping to zero.
    #[error("surface stream sequence space exhausted")]
    SequenceExhausted,
    /// A received frame would move the stream backwards. The receiver drops
    /// it and resets its sequence history rather than displaying stale video.
    #[error("stale or out-of-order sequence {received} after {previous}")]
    StaleSequence { previous: u64, received: u64 },
}

/// One subscriber's inbound queue, bounded at [`DEFAULT_QUEUE_CAP`].
/// Drop-oldest on overflow; `drops` counts each one.
#[derive(Debug)]
struct SubscriberQueue {
    frames: std::collections::VecDeque<SurfaceFrame>,
    cap: usize,
    drops: u64,
}

impl SubscriberQueue {
    fn new(cap: usize) -> Self {
        Self {
            frames: std::collections::VecDeque::with_capacity(cap),
            cap,
            drops: 0,
        }
    }

    fn push(&mut self, frame: SurfaceFrame) {
        while self.frames.len() >= self.cap {
            self.frames.pop_front();
            self.drops = self.drops.saturating_add(1);
        }
        self.frames.push_back(frame);
    }

    fn pop(&mut self) -> Option<SurfaceFrame> {
        self.frames.pop_front()
    }

    fn drops(&self) -> u64 {
        self.drops
    }
}

/// Owner-side sender for one source's video substream. Each subscribed
/// viewer gets an independent queue, so a slow viewer only drops its own frames.
pub struct SurfaceStreamSender {
    pub surface: SurfaceId,
    pub source_peer: PeerId,
    next_sequence: AtomicU64,
    /// Codec and display size stamped on every published frame.
    format: Mutex<(StreamCodec, Option<(u16, u16)>)>,
    subscribers: Mutex<HashMap<PeerId, SubscriberQueue>>,
    queue_cap: usize,
}

impl SurfaceStreamSender {
    /// Construct a sender with the default per-subscriber queue cap.
    pub fn new(surface: SurfaceId, source_peer: PeerId) -> Self {
        Self::with_queue_cap(surface, source_peer, DEFAULT_QUEUE_CAP)
    }

    /// Construct with a caller-chosen queue cap.
    pub fn with_queue_cap(surface: SurfaceId, source_peer: PeerId, cap: usize) -> Self {
        Self {
            surface,
            source_peer,
            next_sequence: AtomicU64::new(0),
            format: Mutex::new((StreamCodec::H264, None)),
            subscribers: Mutex::new(HashMap::new()),
            queue_cap: cap.max(1),
        }
    }

    /// Set the codec (and, for codecs other than H.264, the display size)
    /// stamped on frames published from now on. A new codec needs a keyframe
    /// first; the caller publishes one next.
    pub fn set_format(&self, codec: StreamCodec, display_size: Option<(u16, u16)>) {
        *self.format.lock_recover() = (codec, display_size);
    }

    /// Registers a viewer; idempotent — resets the drop counter and empties the queue if already subscribed.
    pub fn subscribe(&self, peer: PeerId) {
        let mut subs = self.subscribers.lock_recover();
        subs.insert(peer, SubscriberQueue::new(self.queue_cap));
    }

    pub fn unsubscribe(&self, peer: PeerId) {
        let mut subs = self.subscribers.lock_recover();
        subs.remove(&peer);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock_recover().len()
    }

    /// Ships one frame to every subscribed viewer. The sequence counter
    /// advances once regardless of viewer count; returns the minted sequence.
    pub fn publish(&self, keyframe: bool, payload: Vec<u8>) -> Result<u64, FrameError> {
        if payload.len() > MAX_ENCODED_FRAME_LEN {
            return Err(FrameError::PayloadTooLarge(payload.len()));
        }
        let sequence = self
            .next_sequence
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| FrameError::SequenceExhausted)?;
        let (codec, display_size) = *self.format.lock_recover();
        let frame = SurfaceFrame {
            sequence,
            keyframe,
            codec,
            display_size,
            payload,
        };
        // Scoped so the lock is released as soon as delivery is done, rather
        // than held until the function returns.
        {
            let mut subs = self.subscribers.lock_recover();
            for queue in subs.values_mut() {
                queue.push(frame.clone());
            }
        }
        Ok(sequence)
    }

    /// Pops the front frame for `peer`, or `None` if unsubscribed / empty.
    pub fn poll(&self, peer: PeerId) -> Option<SurfaceFrame> {
        let mut subs = self.subscribers.lock_recover();
        subs.get_mut(&peer)?.pop()
    }

    /// Cumulative drop count for a subscriber; 0 if not subscribed.
    pub fn drops_for(&self, peer: PeerId) -> u64 {
        let subs = self.subscribers.lock_recover();
        subs.get(&peer).map_or(0, SubscriberQueue::drops)
    }

    pub fn total_drops(&self) -> u64 {
        let subs = self.subscribers.lock_recover();
        subs.values().map(SubscriberQueue::drops).sum()
    }

    pub fn pending_for(&self, peer: PeerId) -> usize {
        let subs = self.subscribers.lock_recover();
        subs.get(&peer).map_or(0, |q| q.frames.len())
    }
}

/// Receiver-side framer pulling whole [`SurfaceFrame`]s off a wire fed via
/// [`Self::push_bytes`]; flags a sequence gap greater than 1 as [`ReceiverEvent::SequenceGap`].
#[derive(Debug)]
pub struct SurfaceStreamReceiver {
    buffer: Vec<u8>,
    sequence: Sequence,
    total_dropped_seq: u64,
}

/// Event emitted by [`SurfaceStreamReceiver::drain`].
#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverEvent {
    Frame(SurfaceFrame),
    /// A sequence gap was observed — frames dropped somewhere between sender and receiver.
    SequenceGap {
        dropped: u64,
        next_sequence: u64,
    },
}

impl SurfaceStreamReceiver {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
            sequence: Sequence::default(),
            total_dropped_seq: 0,
        }
    }

    /// Appends `bytes` to the internal buffer; call [`Self::drain`] afterwards.
    ///
    /// # Errors
    ///
    /// If accepting the bytes would exceed the incomplete-stream budget, the
    /// buffered partial frame is discarded and the caller gets an error. This
    /// prevents a malformed peer from pinning memory or poisoning later data.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        let Some(buffered) = self.buffer.len().checked_add(bytes.len()) else {
            self.buffer.clear();
            return Err(FrameError::BufferedStreamTooLarge(usize::MAX));
        };
        if buffered > MAX_BUFFERED_STREAM_LEN {
            self.buffer.clear();
            return Err(FrameError::BufferedStreamTooLarge(buffered));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    /// Drains every whole frame in the buffer as [`ReceiverEvent`]s, in arrival order.
    ///
    /// # Errors
    ///
    /// Clears any retained partial bytes before returning a malformed-frame
    /// error, so a later call never reprocesses stale input.
    pub fn drain(&mut self) -> Result<Vec<ReceiverEvent>, FrameError> {
        let mut out = Vec::new();
        loop {
            match SurfaceFrame::decode(&self.buffer) {
                Err(error) => {
                    self.buffer.clear();
                    return Err(error);
                }
                Ok(None) => break,
                Ok(Some((frame, consumed))) => {
                    self.buffer.drain(..consumed);
                    match self.sequence.observe(frame.sequence) {
                        Ok(SequenceEvent::Gap { dropped }) => {
                            self.total_dropped_seq = self.total_dropped_seq.saturating_add(dropped);
                            out.push(ReceiverEvent::SequenceGap {
                                dropped,
                                next_sequence: frame.sequence,
                            });
                        }
                        Ok(SequenceEvent::First | SequenceEvent::Continuous) => {}
                        Err(SequenceError::Exhausted) => {
                            self.buffer.clear();
                            return Err(FrameError::SequenceExhausted);
                        }
                        Err(SequenceError::Stale { previous, received }) => {
                            self.buffer.clear();
                            return Err(FrameError::StaleSequence { previous, received });
                        }
                    }
                    out.push(ReceiverEvent::Frame(frame));
                }
            }
        }
        Ok(out)
    }

    /// Cumulative count of frames the receiver detected as missing.
    pub fn dropped_count(&self) -> u64 {
        self.total_dropped_seq
    }
}

impl Default for SurfaceStreamReceiver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// H.264 keeps its byte format, other codecs carry id and display size, an unknown codec id is
    /// refused, and a single frame round-trips.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn wire_frames_carry_their_codec_and_round_trip() {
        h264_frames_keep_the_original_byte_format();
        other_codecs_carry_their_id_and_display_size();
        a_codec_id_this_build_does_not_know_is_refused();
        round_trip_single_frame();
    }

    fn h264_frames_keep_the_original_byte_format() {
        let bytes = SurfaceFrame::h264(5, true, vec![0, 0, 1, 0x65])
            .encode()
            .unwrap();
        assert_eq!(
            bytes,
            [
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                5,
                0,
                0,
                0,
                4,
                FLAG_KEYFRAME,
                0,
                0,
                1,
                0x65
            ],
            "an H.264 frame is exactly what peers without the codec field expect"
        );
    }

    fn other_codecs_carry_their_id_and_display_size() {
        for codec in [StreamCodec::Vp9, StreamCodec::Av1, StreamCodec::HevcMain10] {
            let frame = SurfaceFrame {
                sequence: 9,
                keyframe: false,
                codec,
                display_size: Some((1920, 1080)),
                payload: vec![1, 2, 3],
            };
            let bytes = frame.encode().unwrap();
            let flags = bytes[12];
            assert_ne!(flags & FLAG_CODEC_MASK, 0, "{codec:?} is not codec 0");
            assert_ne!(flags & FLAG_DISPLAY_SIZE, 0);
            let (back, used) = SurfaceFrame::decode(&bytes).unwrap().unwrap();
            assert_eq!(used, bytes.len());
            assert_eq!(back, frame);
        }
    }

    fn a_codec_id_this_build_does_not_know_is_refused() {
        let mut bytes = SurfaceFrame::h264(0, false, vec![1]).encode().unwrap();
        bytes[12] = 7 << 1;
        assert_eq!(
            SurfaceFrame::decode(&bytes),
            Err(FrameError::InvalidFlags(7 << 1))
        );
    }

    /// Every media codec has the id the shared profile assigns it, the frame's
    /// metadata names the same codec, and a reconnected stream starts again at
    /// its own keyframe.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn codec_ids_come_from_the_shared_profile_and_a_reconnect_starts_at_a_keyframe() {
        codec_ids_agree_with_the_shared_profile();
        a_reconnected_stream_starts_again_at_its_own_keyframe();
    }

    fn codec_ids_agree_with_the_shared_profile() {
        for codec in StreamCodec::ALL {
            let display_size = (codec != StreamCodec::H264).then_some((640, 360));
            let frame = SurfaceFrame {
                sequence: 3,
                keyframe: false,
                codec,
                display_size,
                payload: vec![1, 2, 3],
            };
            let bytes = frame.encode().expect("encode");
            let id = Profile::Callstream
                .codec_id(shared_codec(codec))
                .expect("the callstream profile carries every media codec");
            assert_eq!((bytes[12] & FLAG_CODEC_MASK) >> 1, id, "{codec:?}");
            let (back, _) = SurfaceFrame::decode(&bytes)
                .expect("decode")
                .expect("whole frame");
            assert_eq!(back.meta(), frame.meta(), "{codec:?}");
        }
    }

    fn a_reconnected_stream_starts_again_at_its_own_keyframe() {
        let mut rx = SurfaceStreamReceiver::new();
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 7,
                keyframe: true,
                codec: StreamCodec::H264,
                display_size: None,
                payload: vec![1],
            }
            .encode()
            .expect("encode"),
        )
        .expect("push");
        assert!(matches!(
            rx.drain().expect("first stream").as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame { sequence: 7, .. })]
        ));

        // The new stream's first keyframe is older than the replaced stream's
        // history, so the receiver refuses it and forgets that history.
        let reconnect = SurfaceFrame {
            sequence: 0,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![2],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&reconnect).expect("push reconnect keyframe");
        assert_eq!(
            rx.drain(),
            Err(FrameError::StaleSequence {
                previous: 7,
                received: 0,
            })
        );

        // The same keyframe now starts the new stream, and its delta follows.
        rx.push_bytes(&reconnect).expect("push fresh keyframe");
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 1,
                keyframe: false,
                codec: StreamCodec::H264,
                display_size: None,
                payload: vec![3],
            }
            .encode()
            .expect("encode"),
        )
        .expect("push delta");
        let events = rx.drain().expect("fresh stream");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.as_slice(),
            [
                ReceiverEvent::Frame(SurfaceFrame { sequence: 0, .. }),
                ReceiverEvent::Frame(SurfaceFrame { sequence: 1, .. })
            ]
        ));
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "test phase: arithmetic on small fixed test values cannot overflow"
    )]
    fn round_trip_single_frame() {
        let frame = SurfaceFrame {
            sequence: 42,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: b"\x00\x00\x00\x01idr-nalu".to_vec(),
        };
        let bytes = frame.encode().expect("encode");
        assert_eq!(bytes.len(), FRAME_HEADER_LEN + frame.payload.len());
        let (decoded, consumed) = SurfaceFrame::decode(&bytes)
            .expect("decode")
            .expect("has whole frame");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, bytes.len());
    }

    /// Partial bytes wait, frames reassemble across split buffers, oversized or invalid wire
    /// frames are rejected before payload copy, and malformed partial input is bounded and cleared.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_receiver_reassembles_split_input_and_rejects_malformed_frames() {
        partial_bytes_return_none();
        receiver_frames_across_split_buffers();
        oversized_or_invalid_wire_frame_is_rejected_before_payload_copy();
        receiver_bounds_and_clears_malformed_partial_input();
    }

    fn partial_bytes_return_none() {
        let frame = SurfaceFrame {
            sequence: 1,
            keyframe: false,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![0x11, 0x22, 0x33],
        };
        let bytes = frame.encode().expect("encode");
        assert!(
            SurfaceFrame::decode(&bytes[..=FRAME_HEADER_LEN])
                .expect("decode")
                .is_none()
        );
        assert!(SurfaceFrame::decode(&bytes[..5]).expect("decode").is_none());
    }

    /// Drop-oldest backpressure counts drops, each subscriber has its own queue, an unknown
    /// subscriber never stalls publish, unsubscribe stops delivery and clears drops, and an
    /// oversized payload is never encoded or queued.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn subscriber_queues_are_independent_bounded_and_never_stall_publish() {
        drop_oldest_backpressure_and_drop_counter();
        each_subscriber_has_independent_queue();
        unknown_subscriber_never_stalls_publish();
        unsubscribe_stops_delivery_and_clears_drops();
        oversized_payload_is_never_encoded_or_queued();
    }

    fn drop_oldest_backpressure_and_drop_counter() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 2);
        sender.subscribe(200);
        for _ in 0..4 {
            sender.publish(false, vec![1u8; 8]).expect("publish");
        }
        assert_eq!(sender.drops_for(200), 2);
        assert_eq!(sender.pending_for(200), 2);
        let first = sender.poll(200).expect("first pending");
        let second = sender.poll(200).expect("second pending");
        assert_eq!(first.sequence, 2);
        assert_eq!(second.sequence, 3);
        assert!(sender.poll(200).is_none());
    }

    fn each_subscriber_has_independent_queue() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 3);
        sender.subscribe(200);
        sender.subscribe(201);
        for _ in 0..5 {
            sender.publish(false, vec![1u8]).expect("publish");
        }
        while sender.poll(200).is_some() {}
        assert_eq!(sender.pending_for(201), 3);
        assert_eq!(sender.drops_for(201), 2);
        assert_eq!(sender.drops_for(200), 2);
    }

    fn unknown_subscriber_never_stalls_publish() {
        let sender = SurfaceStreamSender::new(1, 100);
        let s0 = sender.publish(true, vec![0; 4]).expect("publish");
        let s1 = sender.publish(false, vec![0; 4]).expect("publish");
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(sender.subscriber_count(), 0);
    }

    /// Gaps surface on the receiver; sender exhaustion fails closed without queuing or wrapping;
    /// the receiver rejects a wrap and a stale sequence without emitting or counting a huge gap.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn sequence_numbers_surface_gaps_and_fail_closed_on_wrap_or_staleness() {
        sequence_gap_surfaces_on_receiver();
        sequence_exhaustion_fails_closed_without_queuing_or_wrapping();
        receiver_rejects_sequence_wrap_instead_of_treating_it_as_continuous();
        receiver_rejects_stale_sequence_without_emitting_or_counting_a_huge_gap();
    }

    fn sequence_gap_surfaces_on_receiver() {
        let mut rx = SurfaceStreamReceiver::new();
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 0,
                keyframe: true,
                codec: StreamCodec::H264,
                display_size: None,
                payload: vec![1],
            }
            .encode()
            .expect("encode"),
        )
        .expect("push");
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 1,
                keyframe: false,
                codec: StreamCodec::H264,
                display_size: None,
                payload: vec![2],
            }
            .encode()
            .expect("encode"),
        )
        .expect("push");
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 5,
                keyframe: false,
                codec: StreamCodec::H264,
                display_size: None,
                payload: vec![3],
            }
            .encode()
            .expect("encode"),
        )
        .expect("push");
        let events = rx.drain().expect("drain");
        assert_eq!(events.len(), 4, "3 frames + 1 gap event");
        assert!(matches!(events[0], ReceiverEvent::Frame(_)));
        assert!(matches!(events[1], ReceiverEvent::Frame(_)));
        assert!(matches!(
            events[2],
            ReceiverEvent::SequenceGap {
                dropped: 3,
                next_sequence: 5
            }
        ));
        assert!(matches!(events[3], ReceiverEvent::Frame(_)));
        assert_eq!(rx.dropped_count(), 3);
    }

    fn receiver_frames_across_split_buffers() {
        let f0 = SurfaceFrame {
            sequence: 0,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![1, 2, 3, 4, 5],
        };
        let f1 = SurfaceFrame {
            sequence: 1,
            keyframe: false,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![9, 8, 7, 6],
        };
        let mut all = f0.encode().expect("encode");
        all.extend_from_slice(&f1.encode().expect("encode"));

        let mut rx = SurfaceStreamReceiver::new();
        for chunk in all.chunks(3) {
            rx.push_bytes(chunk).expect("push");
            let _ = rx.drain().expect("drain ok");
        }
        assert!(rx.drain().expect("drain").is_empty());

        let mut rx2 = SurfaceStreamReceiver::new();
        rx2.push_bytes(&all).expect("push");
        let events = rx2.drain().expect("drain");
        assert_eq!(events.len(), 2);
        match &events[0] {
            ReceiverEvent::Frame(f) => assert_eq!(f, &f0),
            other @ ReceiverEvent::SequenceGap { .. } => panic!("expected Frame, got {other:?}"),
        }
        match &events[1] {
            ReceiverEvent::Frame(f) => assert_eq!(f, &f1),
            other @ ReceiverEvent::SequenceGap { .. } => panic!("expected Frame, got {other:?}"),
        }
    }

    fn unsubscribe_stops_delivery_and_clears_drops() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 3);
        sender.subscribe(200);
        for _ in 0..5 {
            sender.publish(false, vec![1u8]).expect("publish");
        }
        assert_eq!(sender.drops_for(200), 2);
        sender.unsubscribe(200);
        assert_eq!(sender.drops_for(200), 0);
        sender.subscribe(200);
        assert_eq!(sender.pending_for(200), 0);
        assert_eq!(sender.drops_for(200), 0);
    }

    fn oversized_payload_is_never_encoded_or_queued() {
        let frame = SurfaceFrame {
            sequence: 0,
            keyframe: false,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![0; MAX_ENCODED_FRAME_LEN + 1],
        };
        assert_eq!(
            frame.encode(),
            Err(FrameError::PayloadTooLarge(MAX_ENCODED_FRAME_LEN + 1))
        );

        let sender = SurfaceStreamSender::new(1, 1);
        sender.subscribe(2);
        assert_eq!(
            sender.publish(false, frame.payload),
            Err(FrameError::PayloadTooLarge(MAX_ENCODED_FRAME_LEN + 1))
        );
        assert_eq!(sender.pending_for(2), 0);
    }

    fn oversized_or_invalid_wire_frame_is_rejected_before_payload_copy() {
        let mut oversized = Vec::with_capacity(FRAME_HEADER_LEN);
        oversized.extend_from_slice(&7u64.to_be_bytes());
        oversized.extend_from_slice(
            &u32::try_from(MAX_ENCODED_FRAME_LEN + 1)
                .expect("fits")
                .to_be_bytes(),
        );
        oversized.push(0);
        assert_eq!(
            SurfaceFrame::decode(&oversized),
            Err(FrameError::PayloadTooLarge(MAX_ENCODED_FRAME_LEN + 1))
        );

        let mut invalid_flags = SurfaceFrame {
            sequence: 0,
            keyframe: false,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![],
        }
        .encode()
        .expect("encode");
        invalid_flags[12] = 0b1000_0000;
        assert_eq!(
            SurfaceFrame::decode(&invalid_flags),
            Err(FrameError::InvalidFlags(0b1000_0000))
        );
    }

    fn receiver_bounds_and_clears_malformed_partial_input() {
        let mut rx = SurfaceStreamReceiver::new();
        rx.push_bytes(&vec![0; MAX_BUFFERED_STREAM_LEN + 1])
            .expect_err("oversized partial stream must fail");
        assert!(rx.drain().expect("cleared state").is_empty());

        let mut malformed = Vec::with_capacity(FRAME_HEADER_LEN);
        malformed.extend_from_slice(&0u64.to_be_bytes());
        malformed.extend_from_slice(
            &u32::try_from(MAX_ENCODED_FRAME_LEN + 1)
                .expect("fits")
                .to_be_bytes(),
        );
        malformed.push(0);
        rx.push_bytes(&malformed).expect("header itself fits");
        assert_eq!(
            rx.drain(),
            Err(FrameError::PayloadTooLarge(MAX_ENCODED_FRAME_LEN + 1))
        );

        let valid = SurfaceFrame {
            sequence: 1,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![1],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&valid).expect("push after malformed frame");
        assert!(matches!(
            rx.drain().expect("receiver recovers").as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame { sequence: 1, .. })]
        ));
    }

    fn sequence_exhaustion_fails_closed_without_queuing_or_wrapping() {
        let sender = SurfaceStreamSender::new(1, 1);
        sender.subscribe(2);
        sender.next_sequence.store(u64::MAX - 1, Ordering::Release);
        assert_eq!(sender.publish(false, vec![1]), Ok(u64::MAX - 1));
        assert_eq!(
            sender.publish(false, vec![2]),
            Err(FrameError::SequenceExhausted)
        );
        assert_eq!(
            sender.poll(2).map(|frame| frame.sequence),
            Some(u64::MAX - 1)
        );
        assert!(sender.poll(2).is_none());
    }

    fn receiver_rejects_sequence_wrap_instead_of_treating_it_as_continuous() {
        let mut rx = SurfaceStreamReceiver::new();
        let max = SurfaceFrame {
            sequence: u64::MAX,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![1],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&max).expect("push");
        let events = rx.drain().expect("first frame");
        assert!(matches!(
            events.as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame {
                sequence: u64::MAX,
                ..
            })]
        ));

        let wrapped = SurfaceFrame {
            sequence: 0,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![2],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&wrapped).expect("push");
        assert_eq!(rx.drain(), Err(FrameError::SequenceExhausted));

        // The bad frame and sequence history are cleared, so a new stream can
        // start cleanly without releasing the stale wrapped frame.
        rx.push_bytes(&wrapped).expect("push fresh stream");
        let events = rx.drain().expect("fresh stream");
        assert!(matches!(
            events.as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame { sequence: 0, .. })]
        ));
    }

    fn receiver_rejects_stale_sequence_without_emitting_or_counting_a_huge_gap() {
        let mut rx = SurfaceStreamReceiver::new();
        let first = SurfaceFrame {
            sequence: 5,
            keyframe: true,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![1],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&first).expect("push");
        assert!(matches!(
            rx.drain().expect("first frame").as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame { sequence: 5, .. })]
        ));

        let stale = SurfaceFrame {
            sequence: 4,
            keyframe: false,
            codec: StreamCodec::H264,
            display_size: None,
            payload: vec![2],
        }
        .encode()
        .expect("encode");
        rx.push_bytes(&stale).expect("push stale frame");
        assert_eq!(
            rx.drain(),
            Err(FrameError::StaleSequence {
                previous: 5,
                received: 4,
            })
        );
        assert_eq!(rx.dropped_count(), 0);

        // Resetting history lets a newly established stream start without
        // releasing the stale frame that caused the error.
        rx.push_bytes(&stale).expect("push fresh stream");
        assert!(matches!(
            rx.drain().expect("fresh stream").as_slice(),
            [ReceiverEvent::Frame(SurfaceFrame { sequence: 4, .. })]
        ));
    }
}
