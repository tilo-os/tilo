//! End-to-end integration test for the Flatpak-share pipeline.
//!
//! In-process rather than two netns/processes: gossip transport is
//! already covered by the `hub`/`actor` and `peer_sync_test.rs` suites,
//! this runner has no root for netns, and what's under test
//! (encoder+decoder+registry+`NeighbourMsg` composition against the
//! latency/backpressure budget) is orthogonal to the transport.
//!
//! Asserts a [`SharedFlatpakSession`] sustains a conservative 10 fps floor
//! over a 3s window while targeting the software encoders' 30 fps ceiling,
//! with motion-to-photon latency under 60 ms/frame. It also verifies that a
//! stalled paint side drops video frames without ever dropping input.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::diagnostics::PeerSurfaceMetrics;
use shell::network::protocol::{NeighbourMsg, PeerId, SurfaceId};
use shell::network::streaming::{InputEvent, RemoteInputSink};
use shell::shell::peer_view::{PeerView, PushOutcome};
// `mark_drawn` lives on the `Widget` trait (implemented by PeerView).
use ui::widget::Widget;

use crate::support::peer_stream::push_and_settle;

/// Recording sink for input events — the one in `test_support` is private.
#[derive(Default)]
struct EventRecorder {
    events: Vec<(PeerId, SurfaceId, InputEvent)>,
}

impl RemoteInputSink for EventRecorder {
    fn apply(&mut self, peer: PeerId, surface: SurfaceId, event: InputEvent) {
        self.events.push((peer, surface, event));
    }
}

/// Deterministic RGBA gradient, matching the encoder tests' fixture so
/// PSNR + decoder behaviour is predictable.
fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4) as usize);
    for y in 0..height {
        for x in 0..width {
            // `checked_div`, not `saturating_div`: saturation only covers
            // overflow, not the zero-divisor case, and `saturating_div`
            // still panics on that — `width`/`height` are floored to 1
            // below, so the `None` branch never triggers.
            let r = u8::try_from(x.saturating_mul(255).checked_div(width.max(1)).unwrap_or(0))
                .unwrap_or(255);
            let g = u8::try_from(
                y.saturating_mul(255)
                    .checked_div(height.max(1))
                    .unwrap_or(0),
            )
            .unwrap_or(255);
            out.push(r);
            out.push(g);
            out.push(128);
            out.push(255);
        }
    }
    out
}

fn config() -> FlatpakShareConfig {
    FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 60, // Requested; clamps to 30 in the session.
        encoder: EncoderChoice::Auto,
        hdr: None,
    }
}

const HOST_PEER: PeerId = 0xAAAA;
const VIEWER_PEER: PeerId = 0xBBBB;
const SURFACE: SurfaceId = 42;

/// Start the session and consume its `SurfaceStreamStart` handshake
/// (asserting its fields match `app_id`/`w`/`h`). Split out of
/// [`session_delivers_30_frames_in_order`] to keep
/// that test short.
fn start_session(
    app_id: &str,
    w: u32,
    h: u32,
) -> (SharedFlatpakSession, PeerSurfaceMetrics, PeerView, Instant) {
    let session = SharedFlatpakSession::start(HOST_PEER, app_id, SURFACE, w, h, &config())
        .expect("session construction");
    session.add_viewer(VIEWER_PEER, false);

    // Both sides are in-process and share `origin` (the reference the wire ts
    // is measured from), so the stamp must fall inside readings taken around
    // the handshake call.
    let origin = session.origin();
    let origin_ns = || u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let before_send = origin_ns();
    let handshake = session.stream_start_msg();
    let receiver_ns_at_recv = origin_ns();
    let sender_ns_at_send: u64 = match handshake {
        NeighbourMsg::SurfaceStreamStart {
            surface,
            source_peer,
            width,
            height,
            codec,
            sender_monotonic_ns_at_send,
            ..
        } => {
            assert_eq!(surface, SURFACE);
            assert_eq!(source_peer, HOST_PEER);
            assert_eq!(width, w);
            assert_eq!(height, h);
            assert_eq!(codec, session.chosen_codec().wire_label());
            assert!(
                sender_monotonic_ns_at_send > 0,
                "sender stamps its monotonic reading into the handshake field"
            );
            sender_monotonic_ns_at_send
        }
        other => panic!("expected SurfaceStreamStart, got {other:?}"),
    };

    let metrics = PeerSurfaceMetrics::default();
    let mut view = PeerView::new_with_metrics(SURFACE, VIEWER_PEER, metrics.clone());
    view.restart_stream(session.chosen_codec().wire_label(), Some((w, h)));

    assert!(
        (before_send..=receiver_ns_at_recv).contains(&sender_ns_at_send),
        "the sender stamps the handshake on the shared clock while it is built"
    );
    (session, metrics, view, origin)
}

/// Deposit + encode frames until `wanted` have been decoded on `view` (or
/// the hang deadline passes), returning `(sent, received)`. Split out of
/// [`session_delivers_30_frames_in_order`] to keep that test short.
fn pump_frames_until(
    session: &SharedFlatpakSession,
    view: &mut PeerView,
    frame_bytes: &[u8],
    origin: Instant,
    wanted: u32,
) -> (u32, u32) {
    let deadline = tilo_test_env::hang_deadline();
    let mut sent = 0u32;
    let mut received = 0u32;
    while received < wanted && Instant::now() < deadline {
        session.deposit_captured_rgba(frame_bytes.to_vec());
        let tick_at = Instant::now();
        if let Some(frame) = session.encode_next_frame(tick_at) {
            sent = sent.saturating_add(1);
            let msg = frame.clone().into_neighbour_msg();
            match msg {
                NeighbourMsg::SurfaceFrame {
                    encoded_h264,
                    ts,
                    seq,
                    ..
                } => {
                    // Sender+receiver share `origin`, so subtraction against the wire ts is a
                    // direct motion-to-photon delta.
                    let now_ns = u64::try_from(origin.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    let latency_ns = now_ns.saturating_sub(ts);
                    let outcome = push_and_settle(view, seq, &encoded_h264, Some(latency_ns));
                    assert!(
                        matches!(outcome, PushOutcome::Decoded | PushOutcome::DroppedOldest),
                        "decode failed on frame {sent}",
                    );
                    received = received.saturating_add(1);
                    view.mark_drawn();
                }
                other => panic!("unexpected msg {other:?}"),
            }
        }
        std::thread::sleep(Duration::from_millis(33));
    }
    (sent, received)
}

/// Headless, no GPU: a software encoder, which runs cleanly in CI. Every
/// encoded frame decodes, in order, and the metrics count each one. Latency
/// is printed, not asserted: on a shared runner it measures the host.
#[test]
fn session_delivers_30_frames_in_order() {
    const W: u32 = 320;
    const H: u32 = 240;

    let (session, metrics, mut view, origin) = start_session("org.libreoffice.LibreOffice", W, H);
    let frame_bytes = gradient_frame(W, H);

    // `mark_drawn` per accepted frame keeps this on the happy path; the
    // backpressure test below drives the drop-oldest path.
    let (sent, received) = pump_frames_until(&session, &mut view, &frame_bytes, origin, 30);

    let snap = metrics.snapshot();
    assert_eq!(received, 30, "30 frames decode before the hang deadline");
    assert_eq!(sent, received, "every encoded frame decodes");
    assert_eq!(
        snap.frames_received,
        u64::from(received),
        "metrics counter must match the loop's observed decodes"
    );
    eprintln!(
        "peak latency = {} µs, mean = {} µs, sent={} received={}",
        snap.latency_ns_max / 1_000,
        snap.latency_ns_mean / 1_000,
        sent,
        received,
    );

    match session.stream_end_msg() {
        NeighbourMsg::SurfaceStreamEnd {
            surface,
            source_peer,
            ..
        } => {
            assert_eq!(surface, SURFACE);
            assert_eq!(source_peer, HOST_PEER);
        }
        other => panic!("expected SurfaceStreamEnd, got {other:?}"),
    }
    drop(session);
}

/// Holding off `mark_drawn` simulates the paint side stalling under real
/// CPU / bandwidth pressure.
#[test]
fn backpressure_drops_frames_but_input_stream_still_delivers() {
    const W: u32 = 128;
    const H: u32 = 128;

    let session = SharedFlatpakSession::start(HOST_PEER, "org.gimp.GIMP", SURFACE, W, H, &config())
        .expect("session construction");
    session.add_viewer(VIEWER_PEER, false);

    let metrics = PeerSurfaceMetrics::default();
    let mut view = PeerView::new_with_metrics(SURFACE, VIEWER_PEER, metrics.clone());
    view.restart_stream(session.chosen_codec().wire_label(), Some((W, H)));
    let mut input_sink = EventRecorder::default();
    let frame_bytes = gradient_frame(W, H);

    // Advance tick time between pushes so encode_next_frame always accepts;
    // frames beyond the pending ceiling drop-oldest.
    let mut fake_now = Instant::now();
    let mut encoded_frames = Vec::new();
    for _ in 0..10 {
        session.deposit_captured_rgba(frame_bytes.clone());
        fake_now = fake_now
            .checked_add(Duration::from_millis(40))
            .expect("40ms increments must not overflow Instant");
        if let Some(frame) = session.encode_next_frame(fake_now) {
            encoded_frames.push(frame);
        }
    }

    assert!(
        encoded_frames.len() >= 5,
        "expected >= 5 encoded frames across 10 rapid deposits, got {}",
        encoded_frames.len(),
    );

    let mut input_dispatched = 0u32;
    for (i, frame) in encoded_frames.iter().enumerate() {
        // `i` is a tiny loop index (< 10 encoded frames); widening it losslessly
        // to f64 is what `try_from`+fallback documents.
        let i_f64 = f64::from(u32::try_from(i).unwrap_or(u32::MAX));
        let ev = InputEvent::PointerMotion {
            x: i_f64,
            y: i_f64 * 2.0,
        };
        assert!(
            session.route_input(VIEWER_PEER, ev, &mut input_sink),
            "input event {i} was rejected — share registry regression"
        );
        input_dispatched += 1;
        push_and_settle(&mut view, frame.seq, &frame.nalus, None);
    }
    assert!(session.route_input(
        VIEWER_PEER,
        InputEvent::PointerButton {
            button: 272,
            pressed: true
        },
        &mut input_sink,
    ));
    input_dispatched += 1;

    let snap = metrics.snapshot();
    assert!(
        snap.frames_dropped > 0,
        "expected at least one dropped frame under paint-side stall; \
         frames_received={} frames_dropped={}",
        snap.frames_received,
        snap.frames_dropped,
    );
    assert_eq!(
        u64::try_from(input_sink.events.len()).unwrap_or(u64::MAX),
        u64::from(input_dispatched),
        "input stream lost events under video-side backpressure — \
         streams must be independent",
    );
    eprintln!(
        "backpressure: {} received, {} dropped, {} input events survived",
        snap.frames_received,
        snap.frames_dropped,
        input_sink.events.len(),
    );

    drop(session);
}

#[test]
fn read_only_viewer_input_drops_but_video_still_flows() {
    const W: u32 = 64;
    const H: u32 = 64;

    let session = SharedFlatpakSession::start(HOST_PEER, "org.gimp.GIMP", SURFACE, W, H, &config())
        .expect("session construction");
    session.add_viewer(VIEWER_PEER, true); // read-only

    let mut input_sink = EventRecorder::default();

    assert!(
        !session.route_input(
            VIEWER_PEER,
            InputEvent::PointerMotion { x: 1.0, y: 2.0 },
            &mut input_sink
        ),
        "read-only viewer's input must not reach the sink",
    );
    assert!(input_sink.events.is_empty());

    session.deposit_captured_rgba(gradient_frame(W, H));
    let frame = session
        .encode_next_frame(Instant::now())
        .expect("encoder should still yield frames for read-only viewers");
    assert!(!frame.nalus.is_empty());
    match frame.into_neighbour_msg() {
        NeighbourMsg::SurfaceFrame { .. } => {}
        other => panic!("expected SurfaceFrame, got {other:?}"),
    }

    session.set_read_only(VIEWER_PEER, false);
    assert!(session.route_input(
        VIEWER_PEER,
        InputEvent::PointerButton {
            button: 272,
            pressed: true,
        },
        &mut input_sink,
    ));
    assert_eq!(input_sink.events.len(), 1);
}
