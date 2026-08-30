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
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FRAME_HEADER_LEN + self.payload.len());
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        let len: u32 = self.payload.len().try_into().unwrap_or(u32::MAX);
        buf.extend_from_slice(&len.to_be_bytes());
        let flags = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        buf.push(flags);
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Parses one frame off the front of `input`. Returns the frame and bytes
    /// consumed, or `None` if `input` doesn't yet hold a full frame.
    pub fn decode(input: &[u8]) -> Result<Option<(SurfaceFrame, usize)>, FrameError> {
        if input.len() < FRAME_HEADER_LEN {
            return Ok(None);
        }
        let sequence = u64::from_be_bytes(input[0..8].try_into().unwrap());
        let length = u32::from_be_bytes(input[8..12].try_into().unwrap()) as usize;
        let flags = input[12];
        let total = FRAME_HEADER_LEN + length;
        if length > (u32::MAX as usize) {
            return Err(FrameError::LengthOverflow(length));
        }
        if input.len() < total {
            return Ok(None);
        }
        let payload = input[FRAME_HEADER_LEN..total].to_vec();
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
    /// The length prefix exceeded `u32::MAX`; in practice the header was corrupt.
    #[error("frame length {0} overflowed u32")]
    LengthOverflow(usize),
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
            self.drops += 1;
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
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.insert(peer, SubscriberQueue::new(self.queue_cap));
    }

    pub fn unsubscribe(&self, peer: PeerId) {
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.remove(&peer);
    }

    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("subscribers").len()
    }

    /// Ships one frame to every subscribed viewer. The sequence counter
    /// advances once regardless of viewer count; returns the minted sequence.
    pub fn publish(&self, keyframe: bool, payload: Vec<u8>) -> u64 {
        let sequence = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let frame = SurfaceFrame {
            sequence,
            keyframe,
            payload,
        };
        let mut subs = self.subscribers.lock().expect("subscribers");
        for queue in subs.values_mut() {
            queue.push(frame.clone());
        }
        sequence
    }

    /// Pops the front frame for `peer`, or `None` if unsubscribed / empty.
    pub fn poll(&self, peer: PeerId) -> Option<SurfaceFrame> {
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.get_mut(&peer)?.pop()
    }

    /// Cumulative drop count for a subscriber; 0 if not subscribed.
    pub fn drops_for(&self, peer: PeerId) -> u64 {
        let subs = self.subscribers.lock().expect("subscribers");
        subs.get(&peer).map_or(0, SubscriberQueue::drops)
    }

    pub fn total_drops(&self) -> u64 {
        let subs = self.subscribers.lock().expect("subscribers");
        subs.values().map(SubscriberQueue::drops).sum()
    }

    pub fn pending_for(&self, peer: PeerId) -> usize {
        let subs = self.subscribers.lock().expect("subscribers");
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
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Drains every whole frame in the buffer as [`ReceiverEvent`]s, in arrival order.
    pub fn drain(&mut self) -> Result<Vec<ReceiverEvent>, FrameError> {
        let mut out = Vec::new();
        loop {
            match SurfaceFrame::decode(&self.buffer)? {
                None => break,
                Some((frame, consumed)) => {
                    self.buffer.drain(..consumed);
                    if let Some(prev) = self.last_sequence {
                        let expected = prev.wrapping_add(1);
                        if frame.sequence != expected {
                            let dropped = frame.sequence.wrapping_sub(expected);
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
        let bytes = frame.encode();
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
        let bytes = frame.encode();
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
            sender.publish(false, vec![1u8; 8]);
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
            sender.publish(false, vec![1u8]);
        }
        while sender.poll(200).is_some() {}
        assert_eq!(sender.pending_for(201), 3);
        assert_eq!(sender.drops_for(201), 2);
        assert_eq!(sender.drops_for(200), 2);
    }

    #[test]
    fn unknown_subscriber_never_stalls_publish() {
        let sender = SurfaceStreamSender::new(1, 100);
        let s0 = sender.publish(true, vec![0; 4]);
        let s1 = sender.publish(false, vec![0; 4]);
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
            .encode(),
        );
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 1,
                keyframe: false,
                payload: vec![2],
            }
            .encode(),
        );
        rx.push_bytes(
            &SurfaceFrame {
                sequence: 5,
                keyframe: false,
                payload: vec![3],
            }
            .encode(),
        );
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
        let mut all = f0.encode();
        all.extend_from_slice(&f1.encode());

        let mut rx = SurfaceStreamReceiver::new();
        for chunk in all.chunks(3) {
            rx.push_bytes(chunk);
            let _ = rx.drain().expect("drain ok");
        }
        assert!(rx.drain().expect("drain").is_empty());

        let mut rx2 = SurfaceStreamReceiver::new();
        rx2.push_bytes(&all);
        let events = rx2.drain().expect("drain");
        assert_eq!(events.len(), 2);
        match &events[0] {
            ReceiverEvent::Frame(f) => assert_eq!(f, &f0),
            other => panic!("expected Frame, got {other:?}"),
        }
        match &events[1] {
            ReceiverEvent::Frame(f) => assert_eq!(f, &f1),
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    #[test]
    fn unsubscribe_stops_delivery_and_clears_drops() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 3);
        sender.subscribe(200);
        for _ in 0..5 {
            sender.publish(false, vec![1u8]);
        }
        assert_eq!(sender.drops_for(200), 2);
        sender.unsubscribe(200);
        assert_eq!(sender.drops_for(200), 0);
        sender.subscribe(200);
        assert_eq!(sender.pending_for(200), 0);
        assert_eq!(sender.drops_for(200), 0);
    }
}
