//! End-to-end substream + backpressure test.
//!
//! Host A opens a share, host B subscribes as a viewer. Video frames flow
//! through the dedicated substream, not `NeighbourMsg::SurfaceFrame`
//! gossip — under paint-side stall the sender's bounded queue drops old
//! frames and increments the drop counter, while gossip still carries
//! the `SurfaceStreamStart`/`SurfaceStreamEnd` announcements.
//!
//! In-process, no real Iroh: substream framing + backpressure is
//! transport-agnostic. Real QUIC lives in `two_netns_share.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

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
        encoder: EncoderChoice::Software,
        hdr: None,
    }
}

fn gradient(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4) as usize);
    for y in 0..height {
        for x in 0..width {
            // `checked_div`, not `saturating_div`: saturation only covers
            // overflow, not the zero-divisor case, and `saturating_div`
            // still panics on that — `width`/`height` are floored to 1
            // below, so the `None` branch never triggers.
            out.push(
                u8::try_from(x.saturating_mul(255).checked_div(width.max(1)).unwrap_or(0))
                    .unwrap_or(255),
            );
            out.push(
                u8::try_from(
                    y.saturating_mul(255)
                        .checked_div(height.max(1))
                        .unwrap_or(0),
                )
                .unwrap_or(255),
            );
            out.push(0x80);
            out.push(0xFF);
        }
    }
    out
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn substreams_carry_frames_while_gossip_carries_announcements_until_the_viewer_drops() {
    // Phase `substream_carries_frames_gossip_carries_announcements`.
    // Frames drain in order with monotonic sequence numbers.
    {
        const W: u32 = 128;
        const H: u32 = 128;
        let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, &cfg())
            .expect("session");
        session.add_viewer(VIEWER_A, false);

        match session.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                surface,
                source_peer,
                ..
            } => {
                assert_eq!(surface, SURFACE);
                assert_eq!(source_peer, HOST);
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }

        // Advance past the 33ms (30fps) throttle each tick so every encode accepts.
        let base = Instant::now();
        for i in 0..3u64 {
            session.deposit_captured_rgba(gradient(W, H));
            let now = base
                .checked_add(std::time::Duration::from_millis(
                    100u64.saturating_mul(i.saturating_add(1)),
                ))
                .expect("100ms ticks from base must not overflow Instant");
            let frame = session.encode_next_frame(now);
            assert!(frame.is_some(), "encoder should accept tick {i}");
        }

        // Poll frames out — production would ride a QUIC substream instead.
        let f0 = session.poll_stream_frame(VIEWER_A).expect("frame 0");
        let f1 = session.poll_stream_frame(VIEWER_A).expect("frame 1");
        let f2 = session.poll_stream_frame(VIEWER_A).expect("frame 2");
        assert_eq!(f0.sequence, 0);
        assert_eq!(f1.sequence, 1);
        assert_eq!(f2.sequence, 2);
        assert!(session.poll_stream_frame(VIEWER_A).is_none());
        assert_eq!(session.stream_drops_for(VIEWER_A), 0);

        // Production would write_all(f.encode()) on the wire; push_bytes matches.
        let mut rx = SurfaceStreamReceiver::new();
        for f in [&f0, &f1, &f2] {
            rx.push_bytes(&f.encode().expect("test frame fits wire limit"))
                .expect("bounded test input");
        }
        let events = rx.drain().expect("drain");
        assert_eq!(events.len(), 3, "no sequence gaps expected");
        for (i, ev) in events.iter().enumerate() {
            match ev {
                ReceiverEvent::Frame(f) => assert_eq!(f.sequence, i as u64),
                other @ ReceiverEvent::SequenceGap { .. } => {
                    panic!("expected Frame, got {other:?}")
                }
            }
        }

        match session.stream_end_msg() {
            NeighbourMsg::SurfaceStreamEnd {
                surface,
                source_peer,
                ..
            } => {
                assert_eq!(surface, SURFACE);
                assert_eq!(source_peer, HOST);
            }
            other => panic!("expected SurfaceStreamEnd, got {other:?}"),
        }
    }

    // Phase `drop_viewer_closes_substream_subscription`.
    {
        const W: u32 = 32;
        const H: u32 = 32;
        let session =
            SharedFlatpakSession::start(HOST, "x", SURFACE, W, H, &cfg()).expect("session");
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
            Instant::now()
                .checked_add(std::time::Duration::from_millis(200))
                .expect("200ms from now must not overflow Instant"),
        );
        assert_eq!(session.stream_total_drops(), 0);
    }
}

#[test]
fn substream_backpressure_is_bounded_and_per_subscriber() {
    // Phase `stalled_viewer_drops_old_frames_via_bounded_queue`.
    {
        const W: u32 = 64;
        const H: u32 = 64;
        let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, &cfg())
            .expect("session");
        session.add_viewer(VIEWER_A, false);

        // Deposit + encode 6 frames without polling — cap is 3, so 3 drops expected.
        let base = Instant::now();
        for i in 0..6u64 {
            session.deposit_captured_rgba(gradient(W, H));
            let now = base
                .checked_add(std::time::Duration::from_millis(
                    50u64.saturating_mul(i.saturating_add(1)),
                ))
                .expect("50ms ticks from base must not overflow Instant");
            session.encode_next_frame(now);
        }

        assert_eq!(session.stream_pending_for(VIEWER_A), 3);
        assert_eq!(session.stream_drops_for(VIEWER_A), 3);

        // Retained frames are the newest 3 — drop-oldest keeps recent motion.
        let mut seq = Vec::new();
        while let Some(f) = session.poll_stream_frame(VIEWER_A) {
            seq.push(f.sequence);
        }
        assert_eq!(seq, vec![3, 4, 5]);
    }

    // Phase `per_subscriber_independent_backpressure`.
    // Draining one subscriber's queue must not affect the other's.
    {
        const W: u32 = 64;
        const H: u32 = 64;
        let session = SharedFlatpakSession::start(HOST, "org.gimp.GIMP", SURFACE, W, H, &cfg())
            .expect("session");
        session.add_viewer(VIEWER_A, false);
        session.add_viewer(VIEWER_B, false);

        // 6 encoded frames; both queues cap at 3.
        let base = Instant::now();
        for i in 0..6u64 {
            session.deposit_captured_rgba(gradient(W, H));
            let now = base
                .checked_add(std::time::Duration::from_millis(
                    50u64.saturating_mul(i.saturating_add(1)),
                ))
                .expect("50ms ticks from base must not overflow Instant");
            session.encode_next_frame(now);
        }

        // Viewer A drains fully; viewer B doesn't touch its queue.
        while session.poll_stream_frame(VIEWER_A).is_some() {}

        assert_eq!(session.stream_pending_for(VIEWER_A), 0);
        assert_eq!(session.stream_pending_for(VIEWER_B), 3);
        assert_eq!(session.stream_drops_for(VIEWER_A), 3);
        assert_eq!(session.stream_drops_for(VIEWER_B), 3);
        // Aggregate: 3 drops per viewer, 2 viewers = 6.
        assert_eq!(session.stream_total_drops(), 6);
    }
}
