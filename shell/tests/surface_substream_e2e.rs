//! End-to-end substream + backpressure test.
//!
//! Host A opens a share. Host B subscribes as a viewer. Video frames
//! flow through the dedicated substream — not `NeighbourMsg::SurfaceFrame`
//! gossipsub. Under paint-side stall the sender's bounded queue drops
//! old frames and increments the drop counter. Gossipsub still carries
//! the `SurfaceStreamStart` / `SurfaceStreamEnd` announcements so peers
//! can discover shares.
//!
//! In-process: A single test process, no real libp2p transport. The
//! substream's framing + backpressure semantics are transport-agnostic;
//! the physical wire (QUIC vs. tokio channel vs. loopback socket) is
//! an adapter concern.
//!
//! A two-netns test that exercises the real QUIC transport lives in
//! `two_netns_share.rs` and is gated on `unshare` privileges.

use std::time::Instant;

use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::network::protocol::{NeighbourMsg, PeerId, SurfaceId};
use shell::network::surface_stream::{ReceiverEvent, SurfaceStreamReceiver};

const HOST: PeerId = 0xAAAA;
const VIEWER_A: PeerId = 0xBBBB;
const VIEWER_B: PeerId = 0xCCCC;
const SURFACE: SurfaceId = 42;

fn cfg() -> FlatpakShareConfig {
    FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 30,
        encoder: EncoderChoice::Openh264,
        hdr: None,
    }
}

fn gradient(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            out.push(((x * 255) / width.max(1)) as u8);
            out.push(((y * 255) / height.max(1)) as u8);
            out.push(0x80);
            out.push(0xFF);
        }
    }
    out
}

/// Every encoded frame lands in the substream. A viewer's poll drains
/// them in order with monotonic sequence numbers. Gossipsub carries
/// only the announcement bracket.
#[test]
fn substream_carries_frames_gossipsub_carries_announcements() {
    const W: u32 = 128;
    const H: u32 = 128;
    let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, cfg())
        .expect("session");
    session.add_viewer(VIEWER_A, false);

    // Gossipsub still carries the announcement bracket.
    match session.stream_start_msg() {
        NeighbourMsg::SurfaceStreamStart { surface, source_peer, .. } => {
            assert_eq!(surface, SURFACE);
            assert_eq!(source_peer, HOST);
        }
        other => panic!("expected SurfaceStreamStart, got {other:?}"),
    }

    // Encode 3 frames. Advance the throttle clock past the 33ms
    // 30 fps interval per tick so all three encode ticks accept.
    let base = Instant::now();
    for i in 0..3 {
        session.deposit_captured_rgba(gradient(W, H));
        let now = base + std::time::Duration::from_millis(100 * (i + 1));
        let frame = session.encode_next_frame(now);
        assert!(frame.is_some(), "encoder should accept tick {i}");
    }

    // The substream carries frames with monotonic sequences. Poll
    // them out (production pathway would ride a QUIC substream).
    let f0 = session.poll_stream_frame(VIEWER_A).expect("frame 0");
    let f1 = session.poll_stream_frame(VIEWER_A).expect("frame 1");
    let f2 = session.poll_stream_frame(VIEWER_A).expect("frame 2");
    assert_eq!(f0.sequence, 0);
    assert_eq!(f1.sequence, 1);
    assert_eq!(f2.sequence, 2);
    // Sender's per-subscriber queue is drained.
    assert!(session.poll_stream_frame(VIEWER_A).is_none());
    assert_eq!(session.stream_drops_for(VIEWER_A), 0);

    // Wire the frames through the receive-side framer end-to-end.
    // A production QUIC substream binding would `write_all(f.encode())`
    // on the wire; the receiver's `push_bytes` matches that shape.
    let mut rx = SurfaceStreamReceiver::new();
    for f in [&f0, &f1, &f2] {
        rx.push_bytes(&f.encode());
    }
    let events = rx.drain().expect("drain");
    assert_eq!(events.len(), 3, "no sequence gaps expected");
    for (i, ev) in events.iter().enumerate() {
        match ev {
            ReceiverEvent::Frame(f) => assert_eq!(f.sequence, i as u64),
            other => panic!("expected Frame, got {other:?}"),
        }
    }

    // End-of-stream announcement is still on gossipsub.
    match session.stream_end_msg() {
        NeighbourMsg::SurfaceStreamEnd { surface, source_peer, .. } => {
            assert_eq!(surface, SURFACE);
            assert_eq!(source_peer, HOST);
        }
        other => panic!("expected SurfaceStreamEnd, got {other:?}"),
    }
}

/// Under sustained encode with a stalled viewer, the sender's bounded
/// queue drops old frames and the drop counter reports each drop.
#[test]
fn stalled_viewer_drops_old_frames_via_bounded_queue() {
    const W: u32 = 64;
    const H: u32 = 64;
    let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, cfg())
        .expect("session");
    session.add_viewer(VIEWER_A, false);

    // Deposit + encode 6 frames without polling — the substream's
    // default cap is 3, so we expect 3 drops.
    let base = Instant::now();
    for i in 0..6 {
        session.deposit_captured_rgba(gradient(W, H));
        let now = base + std::time::Duration::from_millis(50 * (i + 1));
        session.encode_next_frame(now);
    }

    // Cap is 3 → the last 3 frames stay pending, the first 3 dropped.
    assert_eq!(session.stream_pending_for(VIEWER_A), 3);
    assert_eq!(session.stream_drops_for(VIEWER_A), 3);

    // The retained frames are the newest 3 — drop-oldest keeps recent
    // motion, discards stale.
    let mut seq = Vec::new();
    while let Some(f) = session.poll_stream_frame(VIEWER_A) {
        seq.push(f.sequence);
    }
    assert_eq!(seq, vec![3, 4, 5]);
}

/// Two subscribers get independent queues. Draining one doesn't
/// affect the other; a slow subscriber's drop counter increments
/// independently of a fast one.
#[test]
fn per_subscriber_independent_backpressure() {
    const W: u32 = 64;
    const H: u32 = 64;
    let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, cfg())
        .expect("session");
    session.add_viewer(VIEWER_A, false);
    session.add_viewer(VIEWER_B, false);

    // 6 encoded frames; both queues cap at 3.
    let base = Instant::now();
    for i in 0..6 {
        session.deposit_captured_rgba(gradient(W, H));
        let now = base + std::time::Duration::from_millis(50 * (i + 1));
        session.encode_next_frame(now);
    }

    // Viewer A drains fully; viewer B doesn't touch its queue.
    while session.poll_stream_frame(VIEWER_A).is_some() {}

    // Viewer B still has 3 pending. Both had the same drop pattern
    // during publish — 6 pushed with cap 3 → 3 drops each.
    assert_eq!(session.stream_pending_for(VIEWER_A), 0);
    assert_eq!(session.stream_pending_for(VIEWER_B), 3);
    assert_eq!(session.stream_drops_for(VIEWER_A), 3);
    assert_eq!(session.stream_drops_for(VIEWER_B), 3);
    // Aggregate: 3 drops per viewer, 2 viewers = 6.
    assert_eq!(session.stream_total_drops(), 6);
}

/// Dropping a viewer stops delivery.
#[test]
fn drop_viewer_closes_substream_subscription() {
    const W: u32 = 32;
    const H: u32 = 32;
    let session = SharedFlatpakSession::start(HOST, "x", SURFACE, W, H, cfg())
        .expect("session");
    session.add_viewer(VIEWER_A, false);
    session.deposit_captured_rgba(gradient(W, H));
    session.encode_next_frame(Instant::now());
    assert_eq!(session.stream_pending_for(VIEWER_A), 1);

    session.drop_viewer(VIEWER_A);
    // Post-drop: queue empty (peer isn't subscribed).
    assert_eq!(session.stream_pending_for(VIEWER_A), 0);
    // Frame published post-drop lands on nobody — total_drops still 0.
    session.deposit_captured_rgba(gradient(W, H));
    session.encode_next_frame(
        Instant::now() + std::time::Duration::from_millis(200),
    );
    assert_eq!(session.stream_total_drops(), 0);
}
