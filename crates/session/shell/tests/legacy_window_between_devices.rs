//! One legacy window streamed between two devices of one person, over real
//! Iroh endpoints: the host encodes just that window with the encoder the
//! viewer's decode capabilities select, the stream travels the direct media
//! route to that viewer alone, and the viewer decodes it into the exact tab
//! the stream names. Input travels back the other way. A third participant
//! of the same workspace, connected to the host, receives none of it.
//!
//! Both devices run in this process, so they share one monotonic clock and
//! every latency below is exact: from the moment the host captured a frame
//! to the moment the viewer had it decoded, and from the viewer sending an
//! input event to the host receiving it. It prints one `legacy-window`
//! summary line for comparing machines (and encoders) and asserts only
//! budgets a loaded CI runner keeps.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::address_lookup::memory::MemoryLookup;
use iroh::endpoint::presets;
use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::diagnostics::PeerSurfaceMetrics;
use shell::network::streaming::InputEvent;
use shell::network::{Actor, NeighbourMsg, NetCmd, NetEvent};
use shell::shell::peer_view::{PeerView, PushOutcome};
use tokio::sync::mpsc;
use ui::widget::Widget as _;

const WS: u64 = 4242;
/// A tab id another device minted: salt in the high bits.
const TAB: u64 = 0x5EED_0000_0000_0007;
const SURFACE: u64 = 31;
const APP: &str = "org.gnome.TextEditor";
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;
/// The app repaints at 60 Hz for three seconds.
const DAMAGE_INTERVAL: Duration = Duration::from_micros(16_667);
const FRAMES: u32 = 180;
const INPUT_SAMPLES: u8 = 40;

async fn spawn_device() -> (
    mpsc::Sender<NetCmd>,
    mpsc::Receiver<NetEvent>,
    iroh::EndpointAddr,
) {
    let lookup = MemoryLookup::new();
    let endpoint = iroh::Endpoint::builder(presets::N0)
        .address_lookup(lookup.clone())
        .bind()
        .await
        .expect("bind Iroh endpoint");
    let addr = endpoint.addr();
    let (cmd_tx, _join) = Actor::spawn(endpoint, lookup);
    let (ev_tx, ev_rx) = mpsc::channel(256);
    cmd_tx
        .send(NetCmd::JoinWorkspace {
            ws: WS,
            on_event: ev_tx,
        })
        .await
        .expect("join the workspace topic");
    (cmd_tx, ev_rx, addr)
}

async fn wait_joined(events: &mut mpsc::Receiver<NetEvent>) {
    tilo_test_env::within("devices met on the workspace topic", async {
        while let Some(event) = events.recv().await {
            if matches!(event, NetEvent::PeerJoined { .. }) {
                return;
            }
        }
        panic!("event channel closed before the other device joined");
    })
    .await;
}

/// A text-editor-like picture with a caret bar that moves every frame, so
/// every frame is damaged and differs from the last.
fn window_frame(index: u32) -> Vec<u8> {
    let caret = index.saturating_mul(7) % WIDTH;
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let line = (y / 24) % 2 == 0 && x % 97 < 80;
            let shade: u8 = if x.abs_diff(caret) < 3 {
                20
            } else if line {
                90
            } else {
                240
            };
            rgba.extend_from_slice(&[shade, shade, shade.saturating_add(10), 255]);
        }
    }
    rgba
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    sorted[sorted.len().saturating_sub(1).saturating_mul(percent) / 100]
}

#[allow(
    clippy::cast_precision_loss,
    reason = "durations of a few seconds in microseconds are exact in f64"
)]
fn millis(duration: Duration) -> f64 {
    duration.as_micros() as f64 / 1000.0
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario: start, stream, measure, send input back"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_window_streams_between_two_devices() {
    let (host_cmd, mut host_events, host_addr) = spawn_device().await;
    let (viewer_cmd, mut viewer_events, viewer_addr) = spawn_device().await;
    let (bystander_cmd, mut bystander_events, bystander_addr) = spawn_device().await;
    let host_peer = shell::network::behaviour::peer_liveness_handle(&host_addr.id);
    let viewer_peer = shell::network::behaviour::peer_liveness_handle(&viewer_addr.id);
    host_cmd
        .send(NetCmd::Connect {
            addr: viewer_addr.clone(),
        })
        .await
        .unwrap();
    viewer_cmd
        .send(NetCmd::Connect {
            addr: host_addr.clone(),
        })
        .await
        .unwrap();
    host_cmd
        .send(NetCmd::Connect {
            addr: bystander_addr,
        })
        .await
        .unwrap();
    bystander_cmd
        .send(NetCmd::Connect { addr: host_addr })
        .await
        .unwrap();
    wait_joined(&mut host_events).await;
    wait_joined(&mut viewer_events).await;
    wait_joined(&mut bystander_events).await;

    // The viewer decodes what this machine decodes; the session picks the
    // codec both ends handle best, hardware first.
    let receivers = [media::realtime::local_capabilities()];
    let session = SharedFlatpakSession::start_with_metrics(
        host_peer,
        APP,
        SURFACE,
        WIDTH,
        HEIGHT,
        &FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 60,
            encoder: EncoderChoice::Auto,
            hdr: None,
        },
        &receivers,
        None,
    )
    .expect("an encoder for the window")
    .with_tab(TAB);
    session.add_viewer(viewer_peer, false);
    let session = Arc::new(session);
    let origin = session.origin();
    let encoder = format!("{:?}", session.chosen_encoder());
    let codec = session.chosen_codec().wire_label().to_string();

    // The host: announce, then encode each damaged frame as the app paints.
    let pictures = (0..8).map(|i| window_frame(i * 23)).collect::<Vec<_>>();
    let start_sent = Instant::now();
    host_cmd
        .send(NetCmd::PublishSurfaceTo {
            ws: WS,
            msg: session.stream_start_msg(),
            viewers: vec![viewer_peer],
        })
        .await
        .unwrap();
    let producer = {
        let session = Arc::clone(&session);
        let host_cmd = host_cmd.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut encode_times = Vec::new();
            let mut sent = 0_u32;
            let mut first_ts = None;
            for index in 0..FRAMES {
                let due = started + DAMAGE_INTERVAL * index;
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                session.deposit_captured_rgba(pictures[(index % 8) as usize].clone());
                let encoding = Instant::now();
                if let Some(frame) = session.encode_next_frame(encoding) {
                    encode_times.push(encoding.elapsed());
                    sent += 1;
                    first_ts.get_or_insert(frame.capture_ts_ns);
                    host_cmd
                        .blocking_send(NetCmd::PublishSurfaceTo {
                            ws: WS,
                            msg: frame.into_neighbour_msg(),
                            viewers: vec![viewer_peer],
                        })
                        .unwrap();
                }
            }
            (sent, encode_times, first_ts)
        })
    };

    // The viewer: mount the stream in the tab it names, decode every frame.
    let metrics = PeerSurfaceMetrics::default();
    let mut view: Option<PeerView> = None;
    let mut latencies = Vec::new();
    let mut arrivals = Vec::new();
    let mut announced_tab = None;
    let mut before_keyframe = 0_u32;
    let mut first_picture = None;
    let mut first_decoded_ts = None;
    let mut previous_received_ts = None;
    let mut early: Vec<(Vec<u8>, u64, Instant)> = Vec::new();
    let mut pending: Vec<(Vec<u8>, u64, Instant)> = Vec::new();
    let receive_until = Instant::now() + DAMAGE_INTERVAL * FRAMES + Duration::from_secs(2);
    while Instant::now() < receive_until {
        let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(250), viewer_events.recv()).await
        else {
            if producer.is_finished() && view.is_some() {
                break;
            }
            continue;
        };
        let NetEvent::Msg { msg, .. } = event else {
            continue;
        };
        match msg {
            NeighbourMsg::SurfaceStreamStart {
                surface,
                source_peer,
                codec,
                width,
                height,
                tab,
                ..
            } => {
                assert_eq!((surface, source_peer), (SURFACE, host_peer));
                announced_tab = Some(tab);
                let mut fresh = PeerView::new_with_metrics(SURFACE, viewer_peer, metrics.clone());
                fresh.restart_stream(&codec, Some((width, height)));
                view = Some(fresh);
                // Frames that overtook the start are decoded now, as the
                // shell does with the ones it held.
                pending.append(&mut early);
            }
            NeighbourMsg::SurfaceFrame {
                surface,
                encoded_h264,
                ts,
                ..
            } if surface == SURFACE => {
                let frame = (encoded_h264, ts, Instant::now());
                if view.is_none() {
                    early.push(frame);
                } else {
                    pending.push(frame);
                }
            }
            _ => {}
        }
        let Some(view) = view.as_mut() else {
            continue;
        };
        for (encoded, ts, arrived) in std::mem::take(&mut pending) {
            let captured = origin + Duration::from_nanos(ts);
            let previous_ts = previous_received_ts.replace(ts);
            let decode_started = Instant::now();
            let outcome = view.push_encoded_with_latency(&encoded, None);
            let decode_elapsed = decode_started.elapsed();
            // The first frame is the keyframe the host forced for this
            // viewer; one that fails to decode means the viewer had to
            // wait for the next keyframe.
            if latencies.is_empty() && outcome == PushOutcome::DecodeFailed {
                before_keyframe += 1;
                continue;
            }
            assert!(
                matches!(outcome, PushOutcome::Decoded | PushOutcome::DroppedOldest),
                "the viewer decodes every frame once it has a keyframe: \
                 outcome={outcome:?} encoder={encoder} codec={codec} \
                 decoded={} before_keyframe={before_keyframe} \
                 capture_ts_ns={ts} previous_capture_ts_ns={previous_ts:?} \
                 capture_gap_ns={:?} keyframe={} encoded_bytes={} \
                 decode_ms={:.2} queued_ms={:.2}",
                latencies.len(),
                previous_ts.and_then(|previous| ts.checked_sub(previous)),
                media::realtime::is_keyframe(view.codec(), &encoded),
                encoded.len(),
                millis(decode_elapsed),
                millis(decode_started.duration_since(arrived)),
            );
            view.mark_drawn();
            first_picture.get_or_insert_with(|| start_sent.elapsed());
            first_decoded_ts.get_or_insert(ts);
            arrivals.push(arrived);
            latencies.push(captured.elapsed());
        }
    }
    let (sent, mut encode_times, first_sent_ts) = producer.join().unwrap();
    assert_eq!(announced_tab, Some(TAB), "the stream names its tab");

    // Input on the viewer goes back to the host as window-local events.
    let mut input_latencies = Vec::new();
    for sequence in 0..INPUT_SAMPLES {
        let event = postcard::to_allocvec(&InputEvent::PointerMotion {
            x: f64::from(sequence),
            y: 10.0,
        })
        .unwrap();
        let sent_at = Instant::now();
        viewer_cmd
            .send(NetCmd::Publish {
                ws: WS,
                msg: NeighbourMsg::RemoteInput {
                    peer: viewer_peer,
                    surface: SURFACE,
                    event: event.clone(),
                },
            })
            .await
            .unwrap();
        tilo_test_env::within("input reached the host", async {
            loop {
                if let Some(NetEvent::Msg {
                    msg:
                        NeighbourMsg::RemoteInput {
                            peer,
                            surface,
                            event: received,
                        },
                    ..
                }) = host_events.recv().await
                    && (peer, surface) == (viewer_peer, SURFACE)
                    && received == event
                {
                    return;
                }
            }
        })
        .await;
        input_latencies.push(sent_at.elapsed());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    latencies.sort_unstable();
    input_latencies.sort_unstable();
    encode_times.sort_unstable();
    let mut gaps = arrivals
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .collect::<Vec<_>>();
    let span = arrivals
        .last()
        .zip(arrivals.first())
        .map_or(Duration::ZERO, |(last, first)| last.duration_since(*first));
    gaps.sort_unstable();
    let received = u32::try_from(latencies.len()).unwrap();
    let fps = f64::from(received.saturating_sub(1)) / span.as_secs_f64().max(f64::EPSILON);
    eprintln!(
        "legacy-window encoder={encoder} codec={codec} size={WIDTH}x{HEIGHT} sent={sent} \
         received={received} before_keyframe={before_keyframe} \
         first_picture_ms={:.2} fps={fps:.1} \
         encode_ms(p50={:.2} p95={:.2}) \
         glass_ms(p50={:.2} p95={:.2} max={:.2}) \
         frame_gap_ms(p50={:.2} p95={:.2} max={:.2}) \
         input_ms(p50={:.2} p95={:.2} max={:.2})",
        millis(first_picture.unwrap_or_default()),
        millis(percentile(&encode_times, 50)),
        millis(percentile(&encode_times, 95)),
        millis(percentile(&latencies, 50)),
        millis(percentile(&latencies, 95)),
        millis(percentile(&latencies, 100)),
        millis(percentile(&gaps, 50)),
        millis(percentile(&gaps, 95)),
        millis(percentile(&gaps, 100)),
        millis(percentile(&input_latencies, 50)),
        millis(percentile(&input_latencies, 95)),
        millis(percentile(&input_latencies, 100)),
    );

    // The bystander shares the workspace and the host's mesh, yet saw no
    // part of the stream; the same frame sent to the whole workspace does
    // reach it, so it was listening.
    host_cmd
        .send(NetCmd::Publish {
            ws: WS,
            msg: NeighbourMsg::SurfaceFrame {
                surface: SURFACE + 1,
                source_peer: host_peer,
                encoded_h264: vec![0, 0, 0, 1, 9],
                ts: 1,
            },
        })
        .await
        .unwrap();
    tilo_test_env::within("a workspace-wide frame reached the bystander", async {
        while let Some(event) = bystander_events.recv().await {
            if let NetEvent::Msg { msg, .. } = event {
                match msg {
                    NeighbourMsg::SurfaceStreamStart { surface, .. }
                    | NeighbourMsg::SurfaceFrame { surface, .. }
                    | NeighbourMsg::SurfaceStreamEnd { surface, .. }
                        if surface == SURFACE =>
                    {
                        panic!("the window's stream reached a device that was not viewing it");
                    }
                    NeighbourMsg::SurfaceFrame { surface, .. } if surface == SURFACE + 1 => return,
                    _ => {}
                }
            }
        }
        panic!("the bystander's event channel closed");
    })
    .await;

    assert!(
        (received + before_keyframe) * 10 >= sent * 9,
        "the direct route delivers nearly every encoded frame: {received} of {sent}"
    );
    assert_eq!(
        before_keyframe, 0,
        "the first frame the viewer gets is its keyframe"
    );
    assert!(
        first_sent_ts.is_some() && first_decoded_ts == first_sent_ts,
        "the first picture is the first frame the host sent, not one a keyframe interval later: \
         sent {first_sent_ts:?}, decoded {first_decoded_ts:?}"
    );
    // Rate and latency are printed above for a quiet machine to judge; on a
    // shared runner they measure the host's load, not the stream.
    assert_eq!(
        input_latencies.len(),
        usize::from(INPUT_SAMPLES),
        "every input event reached the host"
    );
}

/// A board whose hardware decoder stops at 1280x720 watches a 1920x1080
/// window: the host scales it down before encoding, so every frame the board
/// receives fits its decoder, and the first one is a keyframe.
#[test]
fn a_board_viewer_receives_only_pictures_its_hardware_decoder_takes() {
    let board = media::realtime::CodecCapabilities {
        hardware_decode: [media::StreamCodec::H264].into(),
        hardware_decode_max: [(media::StreamCodec::H264, (1280, 720))].into(),
        software_decode: [media::StreamCodec::H264].into(),
        ..media::realtime::CodecCapabilities::default()
    };
    let session = SharedFlatpakSession::start_with_metrics(
        0xA,
        APP,
        SURFACE,
        1920,
        1080,
        &FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 30,
            encoder: EncoderChoice::Auto,
            hdr: None,
        },
        std::slice::from_ref(&board),
        None,
    )
    .expect("an encoder for the window");
    assert_eq!(session.chosen_codec(), media::StreamCodec::H264);
    assert_eq!(
        session.dimensions(),
        (1920, 1080),
        "captured at the window's size"
    );
    assert_eq!(session.encoded_dimensions(), (1280, 720));
    let NeighbourMsg::SurfaceStreamStart { width, height, .. } = session.stream_start_msg() else {
        panic!("a stream start");
    };
    assert_eq!(
        (width, height),
        (1280, 720),
        "the viewer is told the encoded size"
    );

    session.add_viewer(0xB, false);
    let mut view = PeerView::new_with_metrics(SURFACE, 0xB, PeerSurfaceMetrics::default());
    view.restart_stream("h264", Some((width, height)));
    let picture = window_frame_sized(1920, 1080, 3);
    let mut decoded = 0;
    let mut now = Instant::now();
    for _ in 0..5 {
        session.deposit_captured_rgba(picture.clone());
        now += Duration::from_millis(40);
        let Some(frame) = session.encode_next_frame(now) else {
            continue;
        };
        let NeighbourMsg::SurfaceFrame { encoded_h264, .. } = frame.into_neighbour_msg() else {
            panic!("a frame");
        };
        assert_eq!(
            view.push_encoded_with_latency(&encoded_h264, None),
            PushOutcome::Decoded,
            "every frame decodes, the first one too"
        );
        view.mark_drawn();
        let latest = view.latest_frame().expect("a decoded picture");
        assert!((latest.width, latest.height) <= (1280, 720));
        decoded += 1;
    }
    assert!(decoded >= 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_encoded_backlog_preserves_the_viewers_reference_chain() {
    let (host_cmd, mut host_events, host_addr) = spawn_device().await;
    let (viewer_cmd, mut viewer_events, viewer_addr) = spawn_device().await;
    let host_peer = shell::network::behaviour::peer_liveness_handle(&host_addr.id);
    let viewer_peer = shell::network::behaviour::peer_liveness_handle(&viewer_addr.id);
    host_cmd
        .send(NetCmd::Connect { addr: viewer_addr })
        .await
        .unwrap();
    viewer_cmd
        .send(NetCmd::Connect { addr: host_addr })
        .await
        .unwrap();
    wait_joined(&mut host_events).await;
    wait_joined(&mut viewer_events).await;
    let session = SharedFlatpakSession::start_with_metrics(
        host_peer,
        APP,
        SURFACE,
        WIDTH,
        HEIGHT,
        &FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 60,
            encoder: EncoderChoice::Auto,
            hdr: None,
        },
        &[media::realtime::local_capabilities()],
        None,
    )
    .expect("an encoder for the window");
    session.add_viewer(viewer_peer, false);
    let codec = session.chosen_codec().wire_label();
    let mut view = PeerView::new_with_metrics(SURFACE, viewer_peer, PeerSurfaceMetrics::default());
    view.restart_stream(codec, Some((WIDTH, HEIGHT)));
    let (mut frames, timestamps) = record_encoded_units(&session);
    host_cmd
        .send(NetCmd::PublishSurfaceTo {
            ws: WS,
            msg: session.stream_start_msg(),
            viewers: vec![viewer_peer],
        })
        .await
        .unwrap();
    host_cmd
        .send(NetCmd::PublishSurfaceTo {
            ws: WS,
            msg: frames.remove(0),
            viewers: vec![viewer_peer],
        })
        .await
        .unwrap();
    let (encoded, ts) = next_surface_frame(&mut viewer_events).await;
    assert_eq!(ts, timestamps[0]);
    assert_eq!(
        view.push_encoded_with_latency(&encoded, None),
        PushOutcome::Decoded
    );
    view.mark_drawn();
    for msg in frames {
        host_cmd
            .send(NetCmd::PublishSurfaceTo {
                ws: WS,
                msg,
                viewers: vec![viewer_peer],
            })
            .await
            .unwrap();
    }
    let mut previous_index = 0_usize;
    tilo_test_env::within("the final recorded unit reached the viewer", async {
        loop {
            let (encoded_h264, ts) = next_surface_frame(&mut viewer_events).await;
            let index = timestamps.iter().position(|timestamp| *timestamp == ts).expect("recorded capture timestamp");
            let started = Instant::now();
            let outcome = view.push_encoded_with_latency(&encoded_h264, None);
            eprintln!("legacy-backlog previous_index={previous_index} index={index} outcome={outcome:?} decode_ms={:.2}", millis(started.elapsed()));
            assert!(matches!(outcome, PushOutcome::Decoded | PushOutcome::DroppedOldest),
                "every delivered encoded unit decodes: previous_index={previous_index} index={index} outcome={outcome:?}");
            assert_eq!(index, previous_index.saturating_add(1), "the bounded backlog preserves each encoded reference");
            view.mark_drawn();
            previous_index = index;
            if index == timestamps.len().saturating_sub(1) { return; }
        }
    }).await;
}

fn record_encoded_units(session: &SharedFlatpakSession) -> (Vec<NeighbourMsg>, Vec<u64>) {
    // Encoding is complete before publication: this deterministically exercises
    // queued encoded units, rather than relying on loaded-runner capture jitter.
    let mut frames = Vec::new();
    let mut now = Instant::now();
    for index in 0_u32..24 {
        session.deposit_captured_rgba(window_frame(index.saturating_mul(23)));
        now = now
            .checked_add(Duration::from_millis(40))
            .expect("bounded recording clock");
        frames.push(
            session
                .encode_next_frame(now)
                .expect("one encoded unit")
                .into_neighbour_msg(),
        );
    }
    let timestamps: Vec<_> = frames
        .iter()
        .map(|frame| match frame {
            NeighbourMsg::SurfaceFrame { ts, .. } => *ts,
            _ => panic!("an encoded frame"),
        })
        .collect();
    (frames, timestamps)
}

async fn next_surface_frame(events: &mut mpsc::Receiver<NetEvent>) -> (Vec<u8>, u64) {
    tilo_test_env::within("a recorded surface unit reached the viewer", async {
        while let Some(event) = events.recv().await {
            if let NetEvent::Msg {
                msg:
                    NeighbourMsg::SurfaceFrame {
                        encoded_h264, ts, ..
                    },
                ..
            } = event
            {
                return (encoded_h264, ts);
            }
        }
        panic!("viewer event channel closed");
    })
    .await
}

fn window_frame_sized(width: u32, height: u32, index: u32) -> Vec<u8> {
    let caret = index.saturating_mul(7).checked_rem(width).unwrap_or(0);
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let shade: u8 = if x.abs_diff(caret) < 3 || (y / 24) % 2 == 0 && x % 97 < 80 {
                40
            } else {
                230
            };
            rgba.extend_from_slice(&[shade, shade, shade, 255]);
        }
    }
    rgba
}
