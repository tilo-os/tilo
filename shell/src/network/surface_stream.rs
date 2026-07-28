//! Dedicated per-share substream for encoded video frames.
//!
//! # Why not gossipsub
//!
//! The peer-sync v1 pipeline ships `NeighbourMsg::SurfaceFrame` bytes over
//! gossipsub. Gossipsub is reliable-broadcast: every peer on the topic
//! receives every payload eventually, retransmits fill in gaps, and the
//! mesh's IWANT/IHAVE dance chases missing messages for seconds. That's
//! the wrong shape for video:
//!
//! - **Latency**: gossipsub buffers on the sender + retransmits on the
//!   receiver. A dropped video frame is *not* worth re-delivering —
//!   the next frame is already better data.
//! - **Broadcast fanout**: every peer that joined the workspace topic
//!   receives every video frame, even peers that haven't opened a
//!   PeerView. That wastes bandwidth linearly in workspace size.
//! - **No backpressure**: gossipsub has no per-consumer bounded queue.
//!   A slow viewer stalls the sender's buffer until the mesh forces a
//!   drop, at which point the sender has already spent kernel-buffer
//!   bytes on the payload.
//!
//! A dedicated **QUIC substream per share** solves all three:
//!
//! - Per-share stream is one-way host→viewer. No broadcast fan-out.
//! - Bounded queue per subscriber with drop-oldest backpressure and a
//!   per-stream drop counter. Losing old frames is fine; losing recent
//!   frames stalls the viewer. Drop-oldest matches the video paint-side
//!   contract (`shell::peer_view::PeerView`'s ring buffer).
//! - The substream rides libp2p-quic which is already pulled in
//!   transitively (see `Cargo.lock` — `libp2p-quic 0.13`); we don't
//!   need to add a new dep to open substreams alongside the existing
//!   TCP/yamux transport.
//!
//! # Wire format
//!
//! Each frame is prefixed by a 13-byte header:
//!
//! ```text
//! offset  bytes  field
//! 0       8      sequence   u64 big-endian, per-stream monotonic
//! 8       4      length     u32 big-endian, payload byte count
//! 12      1      flags      bit 0 = keyframe (IDR); bits 1-7 reserved
//! 13      N      payload    N = length bytes of NALU data
//! ```
//!
//! The header is fixed-width so a receiver can frame the stream without
//! a length delimiter; the payload is opaque to this module (H.264
//! Annex-B NALUs in the current pipeline).
//!
//! # Announcements stay on gossipsub
//!
//! `NeighbourMsg::SurfaceStreamStart` and `NeighbourMsg::SurfaceStreamEnd`
//! continue to publish on gossipsub — those are workspace-wide
//! discovery events, not video frames. Peers use the announcement to
//! open the substream. `NeighbourMsg::SurfaceFrame` on gossipsub is
//! superseded by [`SurfaceStreamSender::publish`]; the v1
//! `SurfaceFrame` variant stays wire-compatible so legacy peers keep
//! functioning while the mesh migrates.
//!
//! # Input events stay on gossipsub
//!
//! Input events (`NeighbourMsg::RemoteInput`) also stay on gossipsub
//! for v1 — they are small (< 100 bytes), must arrive, and don't
//! benefit from drop-oldest. A follow-up could move them to a second
//! QUIC substream if the observed WAN latency gains justify the extra
//! plumbing.
//!
//! # In-process substrate
//!
//! The transport-agnostic core (framing + per-subscriber bounded queues
//! + backpressure counters) lives in this module. The actual libp2p
//! substream binding — opening a stream on
//! `libp2p_quic::Config` and pumping bytes through
//! `libp2p_swarm::ConnectionHandler` — is provided by the actor as a
//! thin adapter that reads/writes on the QUIC substream and feeds the
//! bytes into a `SurfaceStreamSender` / `SurfaceStreamReceiver`. The
//! split keeps the core unit-testable without a running swarm.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::network::protocol::{PeerId, SurfaceId};

/// Fixed header size in bytes: u64 sequence + u32 length + u8 flags.
pub const FRAME_HEADER_LEN: usize = 8 + 4 + 1;

/// Bit 0 of the header's `flags` byte — this NALU carries an IDR /
/// keyframe.
pub const FLAG_KEYFRAME: u8 = 0b0000_0001;

/// Bounded frame-queue size per subscriber. 3 frames matches the paint
/// side's tolerance for undrawn buffers before drop-oldest kicks in
/// (see `PeerView`'s pending-ceiling constant); we mirror it here so
/// the substream doesn't over-buffer relative to the render loop.
pub const DEFAULT_QUEUE_CAP: usize = 3;

/// A single frame flowing across the substream.
///
/// Contents are opaque past the header — the sender packs H.264
/// Annex-B NALUs into `payload`; the receiver hands them straight to a
/// [`crate::network::decoder::H264Decoder`] without inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceFrame {
    /// Per-stream monotonically increasing sequence number. Starts at 0
    /// on the first frame after [`SurfaceStreamSender::new`] and
    /// increments once per `publish`. Wraps at u64::MAX (~584 years at
    /// 60 fps — not a practical concern).
    pub sequence: u64,
    /// Whether this frame is an IDR / keyframe — the first frame of an
    /// H.264 GOP. Late-joining viewers must start at a keyframe or the
    /// decoder emits garbage until the next one.
    pub keyframe: bool,
    /// Payload bytes. Opaque to this module.
    pub payload: Vec<u8>,
}

impl SurfaceFrame {
    /// Serialise into the on-wire binary layout described in the
    /// module comment. Cheap (one allocation, one iovec).
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

    /// Parse a single frame off the front of `input`. Returns the
    /// parsed frame + the number of input bytes consumed on success,
    /// or `None` if `input` is shorter than the encoded frame length
    /// (the caller should read more bytes and retry).
    ///
    /// The parser rejects `length` values that would overflow the
    /// input slice — a malformed sender can't force an allocation
    /// larger than the frame it actually shipped.
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
    /// The length prefix exceeded u32::MAX, which the wire format can't
    /// address. In practice this means the header was corrupt.
    #[error("frame length {0} overflowed u32")]
    LengthOverflow(usize),
}

/// One subscriber's inbound queue.
///
/// Bounded at [`DEFAULT_QUEUE_CAP`] frames. Drop-oldest on overflow:
/// `send` pops the front of the queue when the queue is full, so the
/// most-recent frame always lands. `drops` counts each drop so the
/// sender can surface it via diagnostics.
#[derive(Debug)]
struct SubscriberQueue {
    /// Bounded FIFO of pending frames.
    frames: std::collections::VecDeque<SurfaceFrame>,
    /// Maximum in-flight frames before drop-oldest kicks in.
    cap: usize,
    /// Cumulative number of drops on this subscriber. Diagnostic
    /// counter; monotonic — never resets over the lifetime of the
    /// subscriber.
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

/// Owner-side sender for a single surface's video substream.
///
/// One `SurfaceStreamSender` per active share. Each subscribed viewer
/// gets an independent bounded queue — a slow viewer only drops its
/// own frames, not other viewers'. The sequence counter is per-stream,
/// not per-subscriber, so all subscribers see the same monotonic
/// sequence for a given [`SurfaceFrame::sequence`].
pub struct SurfaceStreamSender {
    /// The surface this stream carries. Constant for the stream
    /// lifetime.
    pub surface: SurfaceId,
    /// The host that opened the stream. Constant for the stream
    /// lifetime; matches
    /// [`crate::network::NeighbourMsg::SurfaceStreamStart::source_peer`].
    pub source_peer: PeerId,
    /// Next sequence number to mint. Wrapping increments; overflow at
    /// u64::MAX rolls to 0 but is not a practical concern (~584 years
    /// at 60 fps).
    next_sequence: AtomicU64,
    /// Per-subscriber bounded queues. Independent so a slow viewer
    /// doesn't stall other viewers.
    subscribers: Arc<Mutex<HashMap<PeerId, SubscriberQueue>>>,
    /// Queue capacity for new subscribers. Constant for the sender
    /// lifetime.
    queue_cap: usize,
}

impl SurfaceStreamSender {
    /// Construct a sender with the default per-subscriber queue cap.
    pub fn new(surface: SurfaceId, source_peer: PeerId) -> Self {
        Self::with_queue_cap(surface, source_peer, DEFAULT_QUEUE_CAP)
    }

    /// Construct with a caller-chosen queue cap. Tests use this to
    /// force drop-oldest to fire on a smaller buffer.
    pub fn with_queue_cap(surface: SurfaceId, source_peer: PeerId, cap: usize) -> Self {
        Self {
            surface,
            source_peer,
            next_sequence: AtomicU64::new(0),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            queue_cap: cap.max(1),
        }
    }

    /// Register a viewer. Idempotent — a second `subscribe` for the
    /// same peer resets the drop counter and empties the queue (the
    /// prior subscription is treated as closed).
    pub fn subscribe(&self, peer: PeerId) {
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.insert(peer, SubscriberQueue::new(self.queue_cap));
    }

    /// Drop a viewer.
    pub fn unsubscribe(&self, peer: PeerId) {
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.remove(&peer);
    }

    /// Number of currently subscribed viewers.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().expect("subscribers").len()
    }

    /// Ship one frame to every subscribed viewer. The sequence counter
    /// advances once regardless of how many viewers exist — every
    /// subscriber sees the same sequence for the same source frame.
    ///
    /// Returns the sequence number that was minted, so the caller can
    /// correlate with per-frame diagnostics.
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

    /// Pop the front frame for `peer`, if any. Callers pump this on
    /// their tick to drain the queue; in production the QUIC substream
    /// adapter reads from here and writes onto the wire.
    ///
    /// Returns `None` if the peer isn't subscribed or has no pending
    /// frames.
    pub fn poll(&self, peer: PeerId) -> Option<SurfaceFrame> {
        let mut subs = self.subscribers.lock().expect("subscribers");
        subs.get_mut(&peer)?.pop()
    }

    /// Cumulative drop count for a subscriber. Returns 0 when the peer
    /// isn't subscribed (never was, or already unsubscribed).
    pub fn drops_for(&self, peer: PeerId) -> u64 {
        let subs = self.subscribers.lock().expect("subscribers");
        subs.get(&peer).map(|q| q.drops()).unwrap_or(0)
    }

    /// Total drops across every subscriber. Diagnostic aggregate.
    pub fn total_drops(&self) -> u64 {
        let subs = self.subscribers.lock().expect("subscribers");
        subs.values().map(|q| q.drops()).sum()
    }

    /// Snapshot of the pending queue length for `peer`. Diagnostics
    /// only.
    pub fn pending_for(&self, peer: PeerId) -> usize {
        let subs = self.subscribers.lock().expect("subscribers");
        subs.get(&peer).map(|q| q.frames.len()).unwrap_or(0)
    }
}

/// Receiver-side framer that pulls whole [`SurfaceFrame`]s off a wire.
///
/// The QUIC substream adapter feeds arbitrary byte chunks in via
/// [`Self::push_bytes`]; the receiver buffers them and emits framed
/// [`SurfaceFrame`]s as they become complete. This split keeps
/// framing decoupled from the substream transport.
///
/// A sequence-gap detector emits [`ReceiverEvent::SequenceGap`] when
/// the received sequence jumps by more than 1 — the paint side uses
/// that as a hint to wait for the next keyframe.
#[derive(Debug)]
pub struct SurfaceStreamReceiver {
    buffer: Vec<u8>,
    last_sequence: Option<u64>,
    total_dropped_seq: u64,
}

/// Event emitted by [`SurfaceStreamReceiver::drain`].
#[derive(Debug, Clone, PartialEq)]
pub enum ReceiverEvent {
    /// A complete frame surfaced from the byte stream.
    Frame(SurfaceFrame),
    /// A gap in sequence numbers was observed — some frames dropped
    /// between the sender and the receiver (or the sender's own
    /// drop-oldest queue). `dropped` is how many frames were lost;
    /// `next_sequence` is the sequence of the frame that just arrived.
    SequenceGap { dropped: u64, next_sequence: u64 },
}

impl SurfaceStreamReceiver {
    /// Empty receiver ready to accept the first byte.
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(4096),
            last_sequence: None,
            total_dropped_seq: 0,
        }
    }

    /// Append `bytes` to the internal buffer. Call
    /// [`Self::drain`] afterwards to pull framed events.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Drain every whole frame currently in the buffer. Yields a mix
    /// of [`ReceiverEvent::Frame`] and [`ReceiverEvent::SequenceGap`]
    /// in arrival order.
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
    /// Diagnostic — the paint side already draws whatever it decoded.
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
        // Header alone with truncated payload — not enough.
        assert!(
            SurfaceFrame::decode(&bytes[..FRAME_HEADER_LEN + 1])
                .expect("decode")
                .is_none()
        );
        // Header itself truncated — also not enough.
        assert!(SurfaceFrame::decode(&bytes[..5]).expect("decode").is_none());
    }

    #[test]
    fn drop_oldest_backpressure_and_drop_counter() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 2);
        sender.subscribe(200);
        // 4 pushes with cap 2 → 2 drops, keeping the last 2 frames.
        for _ in 0..4 {
            sender.publish(false, vec![1u8; 8]);
        }
        assert_eq!(sender.drops_for(200), 2);
        assert_eq!(sender.pending_for(200), 2);
        // The retained frames must be the *newest* two — drop-oldest
        // keeps the front of the wave, not the tail.
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
        // Peer 200 drains fully; peer 201 doesn't touch its queue.
        while sender.poll(200).is_some() {}
        // Peer 201 still has 3 pending (cap = 3, 5 published → 2 drops).
        assert_eq!(sender.pending_for(201), 3);
        assert_eq!(sender.drops_for(201), 2);
        // Peer 200's drop counter is the same (they shared the
        // publish-side drop path — 5 published, 3 seats).
        assert_eq!(sender.drops_for(200), 2);
    }

    #[test]
    fn unknown_subscriber_never_stalls_publish() {
        let sender = SurfaceStreamSender::new(1, 100);
        // No subscribers — publish still succeeds and increments the
        // sequence counter.
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
                sequence: 5, /* gap of 3 */
                keyframe: false,
                payload: vec![3],
            }
            .encode(),
        );
        let events = rx.drain().expect("drain");
        assert_eq!(events.len(), 4, "3 frames + 1 gap event");
        assert!(matches!(events[0], ReceiverEvent::Frame(_)));
        assert!(matches!(events[1], ReceiverEvent::Frame(_)));
        assert!(
            matches!(
                events[2],
                ReceiverEvent::SequenceGap {
                    dropped: 3,
                    next_sequence: 5
                }
            ),
            "expected SequenceGap dropped=3 next=5, got {:?}",
            events[2]
        );
        assert!(matches!(events[3], ReceiverEvent::Frame(_)));
        assert_eq!(rx.dropped_count(), 3);
    }

    #[test]
    fn receiver_frames_across_split_buffers() {
        // Splitting the byte stream mid-header + mid-payload must still
        // yield the framed events on the eventual drain — a
        // QUIC substream doesn't deliver frame-aligned chunks.
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
        // Feed in chunks of 3 bytes at a time.
        for chunk in all.chunks(3) {
            rx.push_bytes(chunk);
            // Every drain in between is legal — the partial state must
            // survive intermediate drains that don't yet have whole
            // frames.
            let _ = rx.drain().expect("drain ok");
        }
        // After all bytes fed + one final drain there's nothing left.
        assert!(rx.drain().expect("drain").is_empty());

        // Feed everything at once via a fresh receiver — must produce
        // the same two frames in order.
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
    fn total_drops_aggregates_across_subscribers() {
        let sender = SurfaceStreamSender::with_queue_cap(1, 100, 1);
        sender.subscribe(200);
        sender.subscribe(201);
        // 3 pushes with cap 1 → 2 drops per subscriber → 4 total.
        for _ in 0..3 {
            sender.publish(false, vec![7u8]);
        }
        assert_eq!(sender.drops_for(200), 2);
        assert_eq!(sender.drops_for(201), 2);
        assert_eq!(sender.total_drops(), 4);
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
        // Post-unsubscribe: drops_for reports 0 (peer isn't subscribed).
        assert_eq!(sender.drops_for(200), 0);
        // A re-subscribe starts fresh.
        sender.subscribe(200);
        assert_eq!(sender.pending_for(200), 0);
        assert_eq!(sender.drops_for(200), 0);
    }
}
