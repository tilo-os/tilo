//! Per-share frame-transport substrate for encoded video frames.
//!
//! Video wants drop-oldest, not reliable retransmit: a dropped frame isn't
//! worth re-delivering since the next frame is already better data. Each
//! subscriber gets an independent bounded queue with drop-oldest
//! backpressure and a drop counter; a sequence-gap detector on the receive
//! side tells a late-joining viewer to wait for the next keyframe.
//!
//! Each frame carries a fixed 13-byte header, then an opaque payload
//! (H.264 Annex-B NALUs in the current pipeline):
//!
//! ```text
//! offset  bytes  field
//! 0       8      sequence   u64 big-endian, per-stream monotonic
//! 8       4      length     u32 big-endian, payload byte count
//! 12      1      flags      bit 0 = keyframe (IDR); bits 1-7 reserved
//! 13      N      payload    N = length bytes of NALU data
//! ```
//!
//! The byte transport itself — compositor gossip/QUIC, or the call
//! app's iroh-QUIC media transport — lives outside this module, keeping
//! framing and backpressure unit-testable without a running network stack.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

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
pub const MAX_ENCODED_FRAME_LEN: usize = 8 * 1024 * 1024;

/// Largest amount of incomplete wire data retained for one stream.
///
/// A complete maximum-sized frame fits, but a peer cannot retain arbitrary
/// bytes by declaring a frame and never finishing it.
pub const MAX_BUFFERED_STREAM_LEN: usize = FRAME_HEADER_LEN + MAX_ENCODED_FRAME_LEN;

/// Bit 0 of the header's `flags` byte: this NALU is an IDR / keyframe.
pub const FLAG_KEYFRAME: u8 = 0b0000_0001;

/// Bounded frame-queue size per subscriber. 3 frames matches a typical
/// paint side's tolerance before drop-oldest kicks in.
pub const DEFAULT_QUEUE_CAP: usize = 3;

/// A single frame flowing across the substream. Contents past the header
/// are opaque — payload is packed/consumed without inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    /// Per-stream sequence number; starts at 0 and increments once per `publish`.
    pub sequence: u64,
    /// Whether this is an IDR / keyframe. Late-joining viewers must start at
    /// a keyframe or the decoder emits garbage until the next one.
    pub keyframe: bool,
    pub payload: Vec<u8>,
}

impl SurfaceFrame {
    /// Serialises into the wire format above (one allocation).
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::PayloadTooLarge`] instead of emitting an invalid
    /// length prefix when the payload exceeds [`MAX_ENCODED_FRAME_LEN`].
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        if self.payload.len() > MAX_ENCODED_FRAME_LEN {
            return Err(FrameError::PayloadTooLarge(self.payload.len()));
        }
        let capacity = FRAME_HEADER_LEN
            .checked_add(self.payload.len())
            .ok_or(FrameError::LengthOverflow(self.payload.len()))?;
        let mut buf = Vec::with_capacity(capacity);
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        let len: u32 = self
            .payload
            .len()
            .try_into()
            .map_err(|_| FrameError::PayloadTooLarge(self.payload.len()))?;
        buf.extend_from_slice(&len.to_be_bytes());
        let flags = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        buf.push(flags);
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

        if length > MAX_ENCODED_FRAME_LEN {
            return Err(FrameError::PayloadTooLarge(length));
        }
        if flags & !FLAG_KEYFRAME != 0 {
            return Err(FrameError::InvalidFlags(flags));
        }
        let total = FRAME_HEADER_LEN
            .checked_add(length)
            .ok_or(FrameError::LengthOverflow(length))?;
        if input.len() < total {
            return Ok(None);
        }
        let Some(payload_slice) = input.get(FRAME_HEADER_LEN..total) else {
            return Ok(None);
        };
        let payload = payload_slice.to_vec();
        Ok(Some((
            SurfaceFrame {
                sequence,
                keyframe: flags & FLAG_KEYFRAME != 0,
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
            subscribers: Mutex::new(HashMap::new()),
            queue_cap: cap.max(1),
        }
    }

    /// Registers a viewer; idempotent — resets the drop counter and empties the queue if already subscribed.
    pub fn subscribe(&self, peer: PeerId) {
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.insert(peer, SubscriberQueue::new(self.queue_cap));
    }

    pub fn unsubscribe(&self, peer: PeerId) {
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.remove(&peer);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Ships one frame to every subscribed viewer. The sequence counter
    /// advances once regardless of viewer count; returns the minted sequence.
    ///
    /// # Errors
    ///
    /// Fails closed when the payload is too large or the finite sequence space
    /// is exhausted. In either case no frame is queued and no sequence wraps.
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
        let frame = SurfaceFrame {
            sequence,
            keyframe,
            payload,
        };
        // Scoped so the lock is released as soon as delivery is done, rather
        // than held until the function returns.
        {
            let mut subs = self
                .subscribers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for queue in subs.values_mut() {
                queue.push(frame.clone());
            }
        }
        Ok(sequence)
    }

    /// Pops the front frame for `peer`, or `None` if unsubscribed / empty.
    pub fn poll(&self, peer: PeerId) -> Option<SurfaceFrame> {
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.get_mut(&peer)?.pop()
    }

    /// Cumulative drop count for a subscriber; 0 if not subscribed.
    pub fn drops_for(&self, peer: PeerId) -> u64 {
        let subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.get(&peer).map_or(0, SubscriberQueue::drops)
    }

    pub fn total_drops(&self) -> u64 {
        let subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.values().map(SubscriberQueue::drops).sum()
    }

    pub fn pending_for(&self, peer: PeerId) -> usize {
        let subs = self
            .subscribers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        subs.get(&peer).map_or(0, |q| q.frames.len())
    }
}

/// Receiver-side framer pulling whole [`SurfaceFrame`]s off a wire fed via
/// [`Self::push_bytes`]; flags a sequence gap greater than 1 as [`ReceiverEvent::SequenceGap`].
#[derive(Debug)]
pub struct SurfaceStreamReceiver {
    buffer: Vec<u8>,
    last_sequence: Option<u64>,
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
            last_sequence: None,
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
                    if let Some(prev) = self.last_sequence {
                        let Some(expected) = prev.checked_add(1) else {
                            self.buffer.clear();
                            self.last_sequence = None;
                            return Err(FrameError::SequenceExhausted);
                        };
                        if frame.sequence < expected {
                            self.buffer.clear();
                            self.last_sequence = None;
                            return Err(FrameError::StaleSequence {
                                previous: prev,
                                received: frame.sequence,
                            });
                        }
                        if frame.sequence > expected {
                            let dropped = frame.sequence.checked_sub(expected).ok_or(
                                FrameError::StaleSequence {
                                    previous: prev,
                                    received: frame.sequence,
                                },
                            )?;
                            self.total_dropped_seq = self.total_dropped_seq.saturating_add(dropped);
                            out.push(ReceiverEvent::SequenceGap {
                                dropped,
                                next_sequence: frame.sequence,
                            });
                        }
                    }
                    self.last_sequence = Some(frame.sequence);
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

    #[test]
    fn round_trip_single_frame() {
        let frame = SurfaceFrame {
            sequence: 42,
            keyframe: true,
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

    #[test]
    fn partial_bytes_return_none() {
        let frame = SurfaceFrame {
            sequence: 1,
            keyframe: false,
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

    #[test]
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

    #[test]
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

    #[test]
    fn unknown_subscriber_never_stalls_publish() {
        let sender = SurfaceStreamSender::new(1, 100);
        let s0 = sender.publish(true, vec![0; 4]).expect("publish");
        let s1 = sender.publish(false, vec![0; 4]).expect("publish");
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(sender.subscriber_count(), 0);
    }

    #[test]
    fn sequence_gap_surfaces_on_receiver() {
        let mut rx = SurfaceStreamReceiver::new();
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 0,
                keyframe: true,
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

    #[test]
    fn receiver_frames_across_split_buffers() {
        let f0 = SurfaceFrame {
            sequence: 0,
            keyframe: true,
            payload: vec![1, 2, 3, 4, 5],
        };
        let f1 = SurfaceFrame {
            sequence: 1,
            keyframe: false,
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

    #[test]
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

    #[test]
    fn oversized_payload_is_never_encoded_or_queued() {
        let frame = SurfaceFrame {
            sequence: 0,
            keyframe: false,
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

    #[test]
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
            payload: vec![],
        }
        .encode()
        .expect("encode");
        invalid_flags[12] = 0b10;
        assert_eq!(
            SurfaceFrame::decode(&invalid_flags),
            Err(FrameError::InvalidFlags(0b10))
        );
    }

    #[test]
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

    #[test]
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

    #[test]
    fn receiver_rejects_sequence_wrap_instead_of_treating_it_as_continuous() {
        let mut rx = SurfaceStreamReceiver::new();
        let max = SurfaceFrame {
            sequence: u64::MAX,
            keyframe: true,
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

    #[test]
    fn receiver_rejects_stale_sequence_without_emitting_or_counting_a_huge_gap() {
        let mut rx = SurfaceStreamReceiver::new();
        let first = SurfaceFrame {
            sequence: 5,
            keyframe: true,
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
