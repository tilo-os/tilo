//! Two-network-namespace share pipeline test.
//!
//! A host ([`SharedFlatpakSession`]) and a viewer over a **UDP socket** using
//! the kernel network stack. Frames
//! flow host -> viewer as `SurfaceFrame`s (u64 seq + u32 len + u8 keyframe
//! header + Annex-B NALUs); input events flow back over a second socket.
//!
//! The in-process run checks what the pipeline delivers: frames decoded in
//! order and input applied in order. The timing bounds (video >= 10 fps,
//! end-to-end latency < 100 ms per the peer-sync spec, input round-trip
//! < 50 ms) belong to the two-namespace roles below, run on their own rather
//! than beside the rest of the suite on a shared runner.
//!
//! # Netns setup
//!
//! The default `--include-ignored` run drives both roles as async tasks
//! in the same process (shared netns). For two-netns isolation:
//!
//! ```bash
//! # As root (or unshare -Urn — netns creation needs CAP_NET_ADMIN):
//! sudo ip netns add nsA
//! sudo ip netns add nsB
//! sudo ip link add vethA type veth peer name vethB
//! sudo ip link set vethA netns nsA
//! sudo ip link set vethB netns nsB
//! sudo ip netns exec nsA ip addr add 10.42.0.1/24 dev vethA
//! sudo ip netns exec nsA ip link set vethA up
//! sudo ip netns exec nsA ip link set lo up
//! sudo ip netns exec nsB ip addr add 10.42.0.2/24 dev vethB
//! sudo ip netns exec nsB ip link set vethB up
//! sudo ip netns exec nsB ip link set lo up
//!
//! # Run the host in nsA:
//! sudo ip netns exec nsA env TWO_NETNS_ROLE=host \
//!     TWO_NETNS_VIDEO_ADDR=10.42.0.2:9080 \
//!     TWO_NETNS_INPUT_ADDR=10.42.0.1:9081 \
//!     cargo test -p shell --test it --features netns-e2e --locked \
//!     -- --exact two_netns_share::netns_host_process
//!
//! # Run the viewer in nsB (in another terminal):
//! sudo ip netns exec nsB env TWO_NETNS_ROLE=viewer \
//!     TWO_NETNS_VIDEO_ADDR=10.42.0.2:9080 \
//!     TWO_NETNS_INPUT_ADDR=10.42.0.1:9081 \
//!     cargo test -p shell --test it --features netns-e2e --locked \
//!     -- --exact two_netns_share::netns_viewer_process
//! ```
//!
//! The privileged roles compile only with `--features netns-e2e`; once
//! selected, missing driver variables are hard failures. Ordinary CI runs the
//! in-process variant (real UDP, shared netns), still exercising the substream
//! + framing contract end-to-end without claiming namespace isolation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::network::protocol::{PeerId, SurfaceId};
use shell::network::streaming::{InputEvent, RemoteInputSink};
use shell::network::surface_stream::{
    ReceiverEvent, SurfaceFrame as StreamFrame, SurfaceStreamReceiver,
};
use tilo_callstream::surface_decoder::{DecodedFrame, VideoDecoder};

/// Decode one substream frame, following the codec it names.
fn decode_frame(decoder: &mut VideoDecoder, frame: &StreamFrame) -> Option<DecodedFrame> {
    if decoder.codec() != frame.codec {
        *decoder = VideoDecoder::new(frame.codec, false);
    }
    decoder.decode(&frame.payload)
}

const HOST_PEER: PeerId = 0xAAAA;
const VIEWER_PEER: PeerId = 0xBBBB;
const SURFACE: SurfaceId = 42;
const FRAME_W: u32 = 128;
const FRAME_H: u32 = 128;
/// Frames and inputs the in-process pipeline must carry before it stops.
const FRAMES: u32 = 30;
const INPUTS: usize = 5;

#[cfg(feature = "netns-e2e")]
#[derive(serde::Serialize, serde::Deserialize)]
struct NetnsInputPacket {
    sent_ns: u64,
    event: InputEvent,
}

#[cfg(feature = "netns-e2e")]
fn monotonic_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid writable timespec and CLOCK_MONOTONIC exists
    // on every Linux host capable of running network namespaces.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    assert_eq!(status, 0, "CLOCK_MONOTONIC must be readable");
    u64::try_from(time.tv_sec)
        .unwrap_or_default()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or_default())
}

fn cfg() -> FlatpakShareConfig {
    FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 30,
        encoder: EncoderChoice::Software,
        hdr: None,
    }
}

fn gradient() -> Vec<u8> {
    let mut v = Vec::with_capacity(FRAME_W.saturating_mul(FRAME_H).saturating_mul(4) as usize);
    for y in 0..FRAME_H {
        for x in 0..FRAME_W {
            // `checked_div`, not `saturating_div`: saturation only covers
            // overflow, not the zero-divisor case, and `saturating_div`
            // still panics on that — `FRAME_W`/`FRAME_H` are floored to 1
            // below, so the `None` branch never triggers.
            v.push(
                u8::try_from(
                    x.saturating_mul(255)
                        .checked_div(FRAME_W.max(1))
                        .unwrap_or(0),
                )
                .unwrap_or(255),
            );
            v.push(
                u8::try_from(
                    y.saturating_mul(255)
                        .checked_div(FRAME_H.max(1))
                        .unwrap_or(0),
                )
                .unwrap_or(255),
            );
            v.push(0x80);
            v.push(0xFF);
        }
    }
    v
}

/// Recording sink for the input the host applied.
struct HostInputRecorder {
    applied: Arc<std::sync::Mutex<Vec<InputEvent>>>,
}

impl RemoteInputSink for HostInputRecorder {
    fn apply(&mut self, _peer: PeerId, _surface: SurfaceId, event: InputEvent) {
        self.applied.lock().expect("host input log").push(event);
    }
}

/// Host role for
/// [`in_process_udp_pipeline_delivers_frames_and_input_in_order`]: push
/// encoded frames on `video_addr`, apply the input arriving on
/// `input_sock_host`.
fn run_host_thread(
    host_session: &SharedFlatpakSession,
    host_stop: &AtomicBool,
    input_sock_host: &UdpSocket,
    video_addr: std::net::SocketAddr,
    applied: Arc<std::sync::Mutex<Vec<InputEvent>>>,
) {
    let video_send = UdpSocket::bind("127.0.0.1:0").expect("host video-send socket");
    video_send
        .set_nonblocking(true)
        .expect("nonblocking video-send");
    input_sock_host
        .set_nonblocking(true)
        .expect("nonblocking input-in");
    let mut recorder = HostInputRecorder { applied };

    while !host_stop.load(Ordering::Acquire) {
        host_session.deposit_captured_rgba(gradient());
        let _ = host_session.encode_next_frame(Instant::now());
        while let Some(frame) = host_session.poll_stream_frame(VIEWER_PEER) {
            let bytes = frame.encode().expect("test frame fits wire limit");
            // UDP is bounded per packet — fragment into chunks that fit inside a
            // common MTU (1400 bytes, avoiding IP fragmentation) and frame *inside*
            // the UDP payload so `SurfaceStreamReceiver` re-assembles them; packet
            // boundaries are irrelevant to the framing.
            for chunk in bytes.chunks(1400) {
                let _ = video_send.send_to(chunk, video_addr);
            }
        }

        let mut buf = [0u8; 512];
        while let Ok((n, _from)) = input_sock_host.recv_from(&mut buf) {
            if let Ok(ev) = serde_json::from_slice::<InputEvent>(&buf[..n]) {
                // Route through the session so the read-only gate and origin-tagging
                // match production.
                let _ = host_session.route_input(VIEWER_PEER, ev, &mut recorder);
            }
        }

        // The session's own throttle clamps to its 30 fps target; the tick
        // only keeps the loop from spinning.
        thread::sleep(Duration::from_millis(33));
    }
}

/// What the viewer saw: the sequence of every frame it decoded, and the
/// input it sent in order.
struct ViewerLog {
    decoded: Vec<u64>,
    sent: Vec<InputEvent>,
}

/// Viewer role for
/// [`in_process_udp_pipeline_delivers_frames_and_input_in_order`]: read the
/// video socket, re-frame and decode, and send input back on `input_addr`
/// after each decoded frame.
fn run_viewer_thread(
    video_sock_viewer: &UdpSocket,
    viewer_stop: &AtomicBool,
    decoded_count: &AtomicU32,
    input_addr: std::net::SocketAddr,
) -> ViewerLog {
    video_sock_viewer
        .set_nonblocking(true)
        .expect("nonblocking video-in");
    let input_send = UdpSocket::bind("127.0.0.1:0").expect("viewer input-send socket");

    let mut rx = SurfaceStreamReceiver::new();
    let mut decoder = VideoDecoder::h264();
    let mut log = ViewerLog {
        decoded: Vec::new(),
        sent: Vec::new(),
    };
    let mut recv_buf = [0u8; 2048];

    while !viewer_stop.load(Ordering::Acquire) {
        while let Ok((n, _from)) = video_sock_viewer.recv_from(&mut recv_buf) {
            let _ = rx.push_bytes(&recv_buf[..n]);
        }
        let Ok(events) = rx.drain() else {
            continue;
        };
        for ev in events {
            if let ReceiverEvent::Frame(frame) = ev
                && decode_frame(&mut decoder, &frame).is_some()
            {
                log.decoded.push(frame.sequence);
                decoded_count.fetch_add(1, Ordering::AcqRel);
                let index = u32::try_from(log.sent.len()).unwrap_or(u32::MAX);
                let event = InputEvent::PointerMotion {
                    x: f64::from(index),
                    y: f64::from(index) * 2.0,
                };
                let bytes = serde_json::to_vec(&event).expect("input serde");
                if input_send.send_to(&bytes, input_addr).is_ok() {
                    log.sent.push(event);
                }
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    log
}

/// Host and viewer as threads in one process over real UDP loopback
/// sockets, exercising `SurfaceStream` framing, decode and the input path
/// without privileged netns setup. It runs until the pipeline has carried
/// what it checks, however loaded the machine is; the timing bounds are the
/// two-namespace roles'.
#[test]
fn in_process_udp_pipeline_delivers_frames_and_input_in_order() {
    // The kernel picks free ports; host and viewer exchange them via the
    // addresses `local_addr` returns.
    let video_sock_viewer = UdpSocket::bind("127.0.0.1:0").expect("bind viewer's video-in socket");
    let input_sock_host = UdpSocket::bind("127.0.0.1:0").expect("bind host's input-in socket");
    let video_addr = video_sock_viewer.local_addr().expect("video addr");
    let input_addr = input_sock_host.local_addr().expect("input addr");

    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.libreoffice.LibreOffice",
        SURFACE,
        FRAME_W,
        FRAME_H,
        &cfg(),
    )
    .expect("session");
    session.add_viewer(VIEWER_PEER, false);
    let session = Arc::new(session);

    let stop = Arc::new(AtomicBool::new(false));
    let decoded = Arc::new(AtomicU32::new(0));
    let applied = Arc::new(std::sync::Mutex::new(Vec::<InputEvent>::new()));

    let host_session = session;
    let host_stop = stop.clone();
    let host_applied = applied.clone();
    let host_thread = thread::spawn(move || {
        run_host_thread(
            &host_session,
            &host_stop,
            &input_sock_host,
            video_addr,
            host_applied,
        );
    });
    let viewer_stop = stop.clone();
    let viewer_decoded = decoded.clone();
    let viewer_thread = thread::spawn(move || {
        run_viewer_thread(
            &video_sock_viewer,
            &viewer_stop,
            &viewer_decoded,
            input_addr,
        )
    });

    let deadline = tilo_test_env::hang_deadline();
    while decoded.load(Ordering::Acquire) < FRAMES
        || applied.lock().expect("input log").len() < INPUTS
    {
        assert!(
            Instant::now() < deadline,
            "the pipeline stalled: {} frames decoded, {} inputs applied",
            decoded.load(Ordering::Acquire),
            applied.lock().expect("input log").len(),
        );
        thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::Release);
    let log = viewer_thread.join().expect("viewer");
    host_thread.join().expect("host");

    assert!(
        log.decoded.windows(2).all(|pair| pair[0] < pair[1]),
        "frames decode in the order the host encoded them: {:?}",
        log.decoded
    );
    let applied = std::mem::take(&mut *applied.lock().expect("input log"));
    // UDP may drop a packet; what arrives is applied in the order it was sent.
    let mut sent = log.sent.iter();
    assert!(
        applied
            .iter()
            .all(|event| sent.any(|candidate| candidate == event)),
        "input is applied in the order the viewer sent it"
    );
    eprintln!(
        "in-process pipeline: {} frames decoded, {}/{} inputs applied",
        log.decoded.len(),
        applied.len(),
        log.sent.len()
    );
}

#[cfg(feature = "netns-e2e")]
#[test]
fn netns_host_process() {
    let video_addr = std::env::var("TWO_NETNS_VIDEO_ADDR")
        .expect("TWO_NETNS_VIDEO_ADDR is required; see module header");
    let input_addr = std::env::var("TWO_NETNS_INPUT_ADDR")
        .expect("TWO_NETNS_INPUT_ADDR is required; see module header");
    assert_eq!(std::env::var("TWO_NETNS_ROLE").as_deref(), Ok("host"));

    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.libreoffice.LibreOffice",
        SURFACE,
        FRAME_W,
        FRAME_H,
        &cfg(),
    )
    .expect("session");
    session.add_viewer(VIEWER_PEER, false);

    let video_send = UdpSocket::bind("0.0.0.0:0").expect("video-send bind");
    let input_recv = UdpSocket::bind(&input_addr).expect("input-recv bind");
    input_recv
        .set_nonblocking(true)
        .expect("nonblocking input-recv");

    let deadline = Instant::now()
        .checked_add(Duration::from_secs(4))
        .expect("4s from now must not overflow Instant");
    let mut recorder = HostInputRecorder {
        applied: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let mut input_latencies = Vec::new();

    while Instant::now() < deadline {
        let captured_ns = monotonic_ns();
        session.deposit_captured_rgba(gradient());
        let _ = session.encode_next_frame(Instant::now());
        while let Some(frame) = session.poll_stream_frame(VIEWER_PEER) {
            let bytes = frame.encode().expect("test frame fits wire limit");
            // Repeat the capture timestamp on every datagram. The datagram
            // which completes a framed image therefore carries that image's
            // timestamp even if UDP drops or reorders an earlier chunk.
            for chunk in bytes.chunks(1392) {
                let mut packet = Vec::with_capacity(8 + chunk.len());
                packet.extend_from_slice(&captured_ns.to_be_bytes());
                packet.extend_from_slice(chunk);
                let _ = video_send.send_to(&packet, &video_addr);
            }
        }
        let mut buf = [0u8; 512];
        while let Ok((n, _)) = input_recv.recv_from(&mut buf) {
            if let Ok(packet) = serde_json::from_slice::<NetnsInputPacket>(&buf[..n]) {
                input_latencies.push(monotonic_ns().saturating_sub(packet.sent_ns));
                let _ = session.route_input(VIEWER_PEER, packet.event, &mut recorder);
            }
        }
        thread::sleep(Duration::from_millis(33));
    }
    input_latencies.sort_unstable();
    assert!(input_latencies.len() >= 5, "too few cross-namespace inputs");
    let p95 = input_latencies[input_latencies.len().saturating_mul(95) / 100];
    assert!(
        p95 < 50_000_000,
        "cross-namespace input p95 must stay below 50 ms, got {:.1} ms",
        p95 as f64 / 1_000_000.0
    );
    eprintln!(
        "host applied {} cross-namespace inputs (p95 {:.1} ms)",
        input_latencies.len(),
        p95 as f64 / 1_000_000.0,
    );
}

/// Netns viewer role; same env-var contract as [`netns_host_process`].
#[cfg(feature = "netns-e2e")]
#[test]
fn netns_viewer_process() {
    let video_addr = std::env::var("TWO_NETNS_VIDEO_ADDR")
        .expect("TWO_NETNS_VIDEO_ADDR is required; see module header");
    let input_addr = std::env::var("TWO_NETNS_INPUT_ADDR")
        .expect("TWO_NETNS_INPUT_ADDR is required; see module header");
    assert_eq!(std::env::var("TWO_NETNS_ROLE").as_deref(), Ok("viewer"));

    let video_in = UdpSocket::bind(&video_addr).expect("video-in bind");
    video_in
        .set_nonblocking(true)
        .expect("nonblocking video-in");
    let input_send = UdpSocket::bind("0.0.0.0:0").expect("input-send bind");

    let mut rx = SurfaceStreamReceiver::new();
    let mut decoder = VideoDecoder::h264();
    let mut buf = [0u8; 2048];
    let mut frames_decoded = 0u32;
    let mut video_latencies = Vec::new();
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(4))
        .expect("4s from now must not overflow Instant");
    let mut next_input_at = Instant::now()
        .checked_add(Duration::from_millis(200))
        .expect("200ms from now must not overflow Instant");

    while Instant::now() < deadline {
        while let Ok((n, _)) = video_in.recv_from(&mut buf) {
            assert!(
                n >= 8,
                "timestamped video datagram is shorter than its header"
            );
            let captured_ns = u64::from_be_bytes(buf[..8].try_into().expect("8-byte timestamp"));
            let _ = rx.push_bytes(&buf[8..n]);
            if let Ok(events) = rx.drain() {
                for ev in events {
                    if let ReceiverEvent::Frame(f) = ev {
                        let _ = decode_frame(&mut decoder, &f);
                        frames_decoded = frames_decoded.saturating_add(1);
                        video_latencies.push(monotonic_ns().saturating_sub(captured_ns));
                    }
                }
            }
        }
        if Instant::now() >= next_input_at {
            let ev = InputEvent::PointerMotion {
                x: f64::from(frames_decoded),
                y: 0.0,
            };
            let bytes = serde_json::to_vec(&NetnsInputPacket {
                sent_ns: monotonic_ns(),
                event: ev,
            })
            .expect("serde");
            let _ = input_send.send_to(&bytes, &input_addr);
            next_input_at = Instant::now()
                .checked_add(Duration::from_millis(200))
                .expect("200ms from now must not overflow Instant");
        }
        thread::sleep(Duration::from_millis(5));
    }

    assert!(
        frames_decoded >= 30,
        "expected ≥ 10 fps × 3 s = 30 frames, got {frames_decoded}",
    );
    video_latencies.sort_unstable();
    assert_eq!(video_latencies.len(), frames_decoded as usize);
    let video_p95 = video_latencies[video_latencies.len().saturating_mul(95) / 100];
    assert!(
        video_p95 < 100_000_000,
        "cross-namespace capture-to-decode p95 must stay below 100 ms, got {:.1} ms",
        video_p95 as f64 / 1_000_000.0,
    );
    eprintln!(
        "viewer decoded {frames_decoded} frames in 4 s ({} fps sustained, capture-to-decode p95 {:.1} ms)",
        frames_decoded / 4,
        video_p95 as f64 / 1_000_000.0,
    );
}

/// Runs by default: proves the substream + framing + decoder chain works
/// on real UDP without the 3s measurement window, in under 200 ms.
#[test]
fn udp_smoke_frame_round_trips_via_framer_and_decoder() {
    let viewer_sock = UdpSocket::bind("127.0.0.1:0").expect("viewer sock");
    let addr = viewer_sock.local_addr().expect("addr");
    let sender_sock = UdpSocket::bind("127.0.0.1:0").expect("sender sock");

    let session = SharedFlatpakSession::start(HOST_PEER, "app", SURFACE, FRAME_W, FRAME_H, &cfg())
        .expect("session");
    session.add_viewer(VIEWER_PEER, false);
    session.deposit_captured_rgba(gradient());
    let _ = session.encode_next_frame(Instant::now());
    let frame = session
        .poll_stream_frame(VIEWER_PEER)
        .expect("frame emitted");

    let bytes = frame.encode().expect("test frame fits wire limit");
    for chunk in bytes.chunks(1400) {
        sender_sock.send_to(chunk, addr).expect("udp send");
    }

    viewer_sock
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("timeout");

    let mut rx = SurfaceStreamReceiver::new();
    let mut buf = [0u8; 2048];
    let mut received_frame: Option<StreamFrame> = None;
    let deadline = tilo_test_env::hang_deadline();
    while Instant::now() < deadline && received_frame.is_none() {
        if let Ok((n, _)) = viewer_sock.recv_from(&mut buf) {
            let _ = rx.push_bytes(&buf[..n]);
        }
        if let Ok(events) = rx.drain() {
            for ev in events {
                if let ReceiverEvent::Frame(f) = ev {
                    received_frame = Some(f);
                    break;
                }
            }
        }
    }
    let received = received_frame.expect("the loopback frame never arrived within the hang bound");
    assert_eq!(received.sequence, 0);
    assert_eq!(received.payload, frame.payload);

    let mut decoder = VideoDecoder::h264();
    let decoded_frame =
        decode_frame(&mut decoder, &received).expect("decode UDP-transported frame");
    assert_eq!(decoded_frame.width, FRAME_W);
    assert_eq!(decoded_frame.height, FRAME_H);
}
