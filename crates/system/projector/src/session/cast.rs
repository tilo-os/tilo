//! Google Cast and `FCast` sessions: connect, load the HLS stream, adapt the
//! capture profile.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use fcast_sender_sdk::device::{
    ApplicationInfo, DeviceConnectionState, DeviceEventHandler, DeviceInfo, LoadRequest,
    MediaTrack, MediaTrackType, PlaybackState, ProtocolType, QueueState, ReceiverError, Source,
    TrackList,
};
use tilo_projector::control::ConnectStage;
use tilo_projector::{
    CaptureProfile, DisplayStreamPolicy, ProjectionDevice, ProjectionOptions, ProjectionProtocol,
};

use super::context::SessionContext;
use super::hls::video_ingest_loop;
use super::paths::SessionPaths;
use super::receiver_address;
use crate::error::ProjectorError;
use crate::http::{MediaTelemetry, random_media_token, start_http_server, wake_http_listener};

/// How often a streaming session samples receiver telemetry.
const FEEDBACK_TICK: Duration = Duration::from_millis(100);

struct SessionEvents {
    connected: Mutex<Option<mpsc::Sender<Result<ConnectedSession, String>>>>,
    faults: mpsc::Sender<String>,
}

struct ConnectedSession {
    local_addr: fcast_sender_sdk::IpAddr,
    receiver_resolution: Option<(u32, u32)>,
}

impl SessionEvents {
    fn report(&self, result: Result<ConnectedSession, String>) {
        let Ok(mut connected) = self.connected.lock() else {
            return;
        };
        let sender = connected.take();
        drop(connected);
        if let Some(tx) = sender {
            let _ = tx.send(result);
        }
    }
}

impl DeviceEventHandler for SessionEvents {
    fn connection_state_changed(&self, state: DeviceConnectionState) {
        if let DeviceConnectionState::Connected {
            local_addr,
            capabilities,
            ..
        } = state
        {
            let receiver_resolution = capabilities
                .and_then(|value| value.display)
                .and_then(|display| display.resolution)
                .map(|resolution| (resolution.width, resolution.height));
            self.report(Ok(ConnectedSession {
                local_addr,
                receiver_resolution,
            }));
        }
    }
    fn volume_changed(&self, _volume: f64) {}
    fn time_changed(&self, _time: f64) {}
    fn playback_state_changed(&self, _state: PlaybackState) {}
    fn duration_changed(&self, _duration: f64) {}
    fn speed_changed(&self, _speed: f64) {}
    fn source_changed(&self, _source: Source) {}
    fn playback_stopped(&self) {}
    fn playback_error(&self, message: String) {
        let _ = self.faults.send(message.clone());
        self.report(Err(message));
    }
    fn tracks_available(&self, _tracks: Vec<MediaTrack>) {}
    fn track_selected(&self, _id: Option<u32>, _typ: MediaTrackType) {}
    fn tracks_changed(&self, _tracks: TrackList) {}
    fn queue_changed(&self, _queue: QueueState) {}
    fn command_error(&self, error: ReceiverError) {
        let message = format!("receiver rejected projection: {error:?}");
        let _ = self.faults.send(message.clone());
        self.report(Err(message));
    }
}

fn cast_load(
    device: &Arc<dyn fcast_sender_sdk::device::CastingDevice>,
    url: &str,
) -> Result<(), ProjectorError> {
    device
        .load(
            LoadRequest::Url {
                content_type: "application/vnd.apple.mpegurl".to_owned(),
                url: url.to_owned(),
                resume_position: None,
                speed: None,
                volume: None,
                metadata: Some(fcast_sender_sdk::device::Metadata {
                    title: Some("Tilo screen".to_owned()),
                    thumbnail_url: None,
                }),
                request_headers: None,
            },
            None,
        )
        .map_err(|error| {
            ProjectorError::Receiver(format!("receiver rejected the screen stream: {error}"))
        })
}

/// Scales a capture dimension by `numerator/denominator`, falling back to
/// `fallback` if the multiplication would overflow `u32` or `denominator` is
/// zero. `value` ultimately traces back to a receiver-advertised display
/// capability, so it is untrusted magnitude data; a fallback to an
/// already-known-safe bound is the correct response to a malformed
/// advertisement, not a wraparound or panic.
fn scaled_dimension(value: u32, numerator: u32, denominator: u32, fallback: u32) -> u32 {
    value
        .checked_mul(numerator)
        .and_then(|scaled| scaled.checked_div(denominator))
        .unwrap_or(fallback)
}

fn adapt_capture_profile(
    current: CaptureProfile,
    maximum: CaptureProfile,
    delivered_bits_per_second: u64,
    average_service_us: u64,
) -> CaptureProfile {
    let profile_for = |width: u32, height: u32, fps: u32| {
        let pixels_per_second = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|value| value.checked_mul(u64::from(fps)));
        let bitrate_kbps = pixels_per_second
            .map_or_else(|| u64::from(maximum.bitrate_kbps), |value| value / 12_000)
            .clamp(2_500, u64::from(maximum.bitrate_kbps));
        CaptureProfile::new(
            width,
            height,
            fps,
            u32::try_from(bitrate_kbps).unwrap_or(maximum.bitrate_kbps),
        )
    };
    // `current.bitrate_kbps` is always clamped to at most 40_000 by every
    // constructor of `CaptureProfile` (see `DisplayStreamPolicy::ceiling` and
    // `profile_for` above), so these thresholds cannot realistically
    // overflow; `saturating_mul` is used only to avoid a bare arithmetic
    // operator, not because saturation is expected to occur.
    let required = u64::from(current.bitrate_kbps).saturating_mul(1_000);
    let shrink_threshold = required.saturating_mul(5) / 4;
    let grow_threshold = required.saturating_mul(2);
    if (delivered_bits_per_second > 0 && delivered_bits_per_second < shrink_threshold)
        || average_service_us > 250_000
    {
        let width = scaled_dimension(current.max_width, 3, 4, maximum.max_width).max(640) & !1;
        let height = scaled_dimension(current.max_height, 3, 4, maximum.max_height).max(360) & !1;
        profile_for(width, height, current.fps)
    } else if delivered_bits_per_second > grow_threshold
        && average_service_us < 100_000
        && current != maximum
    {
        let width = scaled_dimension(current.max_width, 4, 3, maximum.max_width)
            .min(maximum.max_width)
            & !1;
        let height = scaled_dimension(current.max_height, 4, 3, maximum.max_height)
            .min(maximum.max_height)
            & !1;
        profile_for(width, height, maximum.fps)
    } else {
        current
    }
}

/// The Cast SDK protocol for a receiver discovered as `protocol`.
fn cast_protocol(protocol: ProjectionProtocol) -> Result<ProtocolType, ProjectorError> {
    match protocol {
        ProjectionProtocol::FCast => Ok(ProtocolType::FCast),
        ProjectionProtocol::Chromecast => Ok(ProtocolType::Chromecast),
        other => Err(ProjectorError::Protocol(format!(
            "{} transport is not implemented by the HLS session",
            other.label()
        ))),
    }
}

/// A connected receiver. It owns the SDK runtime: the device keeps only a
/// handle, and dropping the runtime aborts the worker before the first
/// media LOAD.
struct CastReceiver {
    _runtime: fcast_sender_sdk::context::CastContext,
    device: Arc<dyn fcast_sender_sdk::device::CastingDevice>,
    connected: ConnectedSession,
    faults: mpsc::Receiver<String>,
}

/// Opens the receiver connection for a cast session and waits for the
/// receiver's `Connected` event.
///
/// # Errors
///
/// Returns an error if the Cast context or connection cannot be started, or
/// if the receiver does not accept the connection within 8 seconds.
fn connect_cast_device(
    protocol: ProtocolType,
    address: std::net::IpAddr,
    port: u16,
    name: String,
) -> Result<CastReceiver, ProjectorError> {
    let runtime = fcast_sender_sdk::context::CastContext::new().map_err(|error| {
        ProjectorError::Unavailable(format!("cannot initialise Cast session: {error}"))
    })?;
    let info = DeviceInfo {
        name,
        protocol,
        addresses: vec![fcast_sender_sdk::IpAddr::from(address)],
        port,
        txt_records: HashMap::new(),
    };
    let device = runtime.create_device_from_info(info);
    let (connected_tx, connected_rx) = mpsc::channel();
    let (fault_tx, faults) = mpsc::channel();
    device
        .connect(
            Some(ApplicationInfo {
                name: "tilo-projector".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                display_name: "Tilo Screen Projection".to_owned(),
            }),
            Arc::new(SessionEvents {
                connected: Mutex::new(Some(connected_tx)),
                faults: fault_tx,
            }),
            1_000,
        )
        .map_err(|error| {
            ProjectorError::Receiver(format!("cannot start receiver connection: {error}"))
        })?;
    let connected = connected_rx
        .recv_timeout(Duration::from_secs(8))
        .map_err(|_| ProjectorError::Timeout("receiver did not accept the connection in time"))?
        .map_err(ProjectorError::Receiver)?;
    Ok(CastReceiver {
        _runtime: runtime,
        device,
        connected,
        faults,
    })
}

/// The local half of a cast session: video ingest from the compositor and
/// the token-guarded HLS server the receiver fetches from. Dropping it stops
/// and joins both.
struct MediaServer {
    stop: Arc<AtomicBool>,
    video_socket: PathBuf,
    receiver: std::net::IpAddr,
    port: u16,
    token: String,
    telemetry: Arc<MediaTelemetry>,
    video_thread: Option<JoinHandle<Result<(), ProjectorError>>>,
    http_thread: Option<JoinHandle<()>>,
}

impl MediaServer {
    fn start(
        paths: &SessionPaths,
        receiver: std::net::IpAddr,
        audio: bool,
    ) -> Result<Self, ProjectorError> {
        let stop = Arc::new(AtomicBool::new(false));
        let video_paths = paths.clone();
        let video_stop = stop.clone();
        let video_thread = std::thread::Builder::new()
            .name("tilo-proj-video".to_owned())
            .spawn(move || video_ingest_loop(&video_paths, &video_stop, audio))
            .map_err(ProjectorError::io("cannot start projection video ingest"))?;
        let mut server = Self {
            stop,
            video_socket: paths.video_socket.clone(),
            receiver,
            port: 0,
            token: random_media_token(),
            telemetry: Arc::new(MediaTelemetry::new()),
            video_thread: Some(video_thread),
            http_thread: None,
        };
        // On failure `server` drops here and stops the video ingest.
        let (port, http_thread) = start_http_server(
            paths.root.clone(),
            server.stop.clone(),
            receiver,
            server.token.clone(),
            server.telemetry.clone(),
        )?;
        server.port = port;
        server.http_thread = Some(http_thread);
        Ok(server)
    }

    /// The playlist URL the receiver loads, served at `local_addr`.
    fn playlist_url(&self, local_addr: &fcast_sender_sdk::IpAddr) -> String {
        format!(
            "http://{}:{}/{}/live.m3u8",
            fcast_sender_sdk::url_format_ip_addr(local_addr),
            self.port,
            self.token
        )
    }
}

impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake workers that are waiting in accept.
        let _ = std::os::unix::net::UnixStream::connect(&self.video_socket);
        if let Some(http_thread) = self.http_thread.take() {
            wake_http_listener(self.receiver, self.port);
            let _ = http_thread.join();
        }
        if let Some(video_thread) = self.video_thread.take() {
            let _ = video_thread.join();
        }
    }
}

/// The pieces of a running cast session the adaptive feedback loop needs to
/// read but does not own.
struct CastFeedbackContext<'a> {
    session: &'a SessionContext,
    device: &'a Arc<dyn fcast_sender_sdk::device::CastingDevice>,
    url: &'a str,
    telemetry: &'a MediaTelemetry,
    fault_rx: &'a mpsc::Receiver<String>,
}

/// Watches receiver telemetry until the session is asked to stop, reloading
/// the receiver after a stall and (when `adaptive`) publishing a new capture
/// profile every 3 seconds.
///
/// # Errors
///
/// Returns an error if the receiver stops fetching the stream after three
/// recovery attempts, or if reloading the stream on the receiver fails.
fn run_adaptive_feedback_loop(
    ctx: &CastFeedbackContext<'_>,
    adaptive: bool,
    maximum_profile: CaptureProfile,
    initial_profile: CaptureProfile,
) -> Result<(), ProjectorError> {
    let mut last_feedback = Instant::now();
    let mut last_bytes = 0;
    let mut last_feedback_requests = 0;
    let mut last_service_us = 0;
    let mut last_seen_requests = 0;
    let mut current_profile = initial_profile;
    let mut recovery_attempts = 0u8;
    // The tick samples telemetry; a stop request ends the wait at once.
    while !ctx.session.wait_stop(FEEDBACK_TICK) {
        let requests = ctx.telemetry.requests.load(Ordering::Acquire);
        let now_ms = ctx.telemetry.elapsed_millis();
        let last_request_ms = ctx.telemetry.last_request_ms.load(Ordering::Acquire);
        let receiver_stalled = requests > 0 && now_ms.saturating_sub(last_request_ms) > 6_000;
        let receiver_fault = ctx.fault_rx.try_recv().is_ok();
        if receiver_stalled || receiver_fault {
            if recovery_attempts >= 3 {
                return Err(ProjectorError::Receiver(
                    "receiver stopped fetching the stream after three recovery attempts".to_owned(),
                ));
            }
            cast_load(ctx.device, ctx.url)?;
            recovery_attempts = recovery_attempts.saturating_add(1);
        } else if requests > last_seen_requests {
            recovery_attempts = 0;
        }
        if adaptive && last_feedback.elapsed() >= Duration::from_secs(3) {
            let bytes = ctx.telemetry.bytes.load(Ordering::Relaxed);
            let request_count = ctx.telemetry.requests.load(Ordering::Relaxed);
            let service_us = ctx.telemetry.service_us.load(Ordering::Relaxed);
            let elapsed = last_feedback.elapsed().as_secs_f64().max(0.001);
            // `as` between float and integer types saturates per Rust's
            // language semantics (never UB). This is an approximate bitrate
            // estimate feeding the adaptive-streaming heuristic, not a value
            // used to size or index memory, so the precision trade-off is
            // intentional.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "approximate telemetry conversion; `as` saturates by language definition"
            )]
            let delivered_bps = ((bytes.saturating_sub(last_bytes) as f64 * 8.0) / elapsed) as u64;
            let request_delta = request_count.saturating_sub(last_feedback_requests).max(1);
            let average_service_us = service_us
                .saturating_sub(last_service_us)
                .checked_div(request_delta)
                .unwrap_or(0);
            let next = adapt_capture_profile(
                current_profile,
                maximum_profile,
                delivered_bps,
                average_service_us,
            );
            if next != current_profile {
                ctx.session.publish_profile(next);
                current_profile = next;
            }
            last_bytes = bytes;
            last_feedback_requests = request_count;
            last_service_us = service_us;
            last_feedback = Instant::now();
        }
        last_seen_requests = requests;
    }
    Ok(())
}

/// The profile to start capturing at: below `maximum_profile.ceiling()`, so
/// the receiver sees a first frame quickly, before the feedback loop has any
/// throughput data to adapt from.
fn initial_capture_profile(
    stream_policy: DisplayStreamPolicy,
    maximum_profile: CaptureProfile,
) -> CaptureProfile {
    if stream_policy.adaptive {
        CaptureProfile::new(
            maximum_profile.max_width.min(1280),
            maximum_profile.max_height.min(720),
            maximum_profile.fps,
            maximum_profile.bitrate_kbps.min(6_000),
        )
    } else {
        maximum_profile
    }
}

pub(crate) fn run_cast_session(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    context: &SessionContext,
) -> Result<(), ProjectorError> {
    let protocol = cast_protocol(device.protocol)?;
    let address: std::net::IpAddr = receiver_address(device)?
        .trim_matches(['[', ']'])
        .parse()
        .map_err(|error| ProjectorError::Protocol(format!("invalid receiver address: {error}")))?;
    let stream_policy = options.stream_policy;
    context.publish_profile(initial_capture_profile(
        stream_policy,
        stream_policy.ceiling(None),
    ));
    let media = MediaServer::start(&context.paths, address, options.audio)?;
    context.stage(ConnectStage::Handshake);
    let receiver = connect_cast_device(protocol, address, device.port, device.name.clone())?;
    let streamed = stream_to_receiver(context, &receiver, &media, stream_policy);
    let _ = receiver.device.stop_playback();
    let _ = receiver.device.disconnect();
    drop(media);
    streamed
}

/// Hands the receiver the stream once the compositor's first frames are in
/// it, then keeps it flowing until the session is stopped or fails.
fn stream_to_receiver(
    context: &SessionContext,
    receiver: &CastReceiver,
    media: &MediaServer,
    stream_policy: DisplayStreamPolicy,
) -> Result<(), ProjectorError> {
    let maximum_profile = stream_policy.ceiling(receiver.connected.receiver_resolution);
    let initial_profile = initial_capture_profile(stream_policy, maximum_profile);
    context.publish_profile(initial_profile);

    context.stage(ConnectStage::WaitingForFrames);
    let playlist_deadline = Instant::now()
        .checked_add(Duration::from_secs(8))
        .ok_or_else(|| {
            ProjectorError::Unavailable(
                "system clock cannot represent the playlist deadline".to_owned(),
            )
        })?;
    // The muxer writes the playlist into the runtime directory; a stop
    // request still ends this wait at once.
    while !context.paths.playlist.exists() {
        if Instant::now() >= playlist_deadline {
            return Err(ProjectorError::Timeout(
                "the compositor did not deliver video frames in time",
            ));
        }
        if context.wait_stop(Duration::from_millis(50)) {
            return Ok(());
        }
    }
    let url = media.playlist_url(&receiver.connected.local_addr);
    context.stage(ConnectStage::Loading);
    cast_load(&receiver.device, &url)?;
    context.ready();

    let feedback_ctx = CastFeedbackContext {
        session: context,
        device: &receiver.device,
        url: &url,
        telemetry: &media.telemetry,
        fault_rx: &receiver.faults,
    };
    run_adaptive_feedback_loop(
        &feedback_ctx,
        stream_policy.adaptive,
        maximum_profile,
        initial_profile,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Command, Stdio};

    use tilo_projector::VideoPacket;

    use crate::session::hls::HlsPipeline;

    fn request(port: u16, path: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .expect("connect media server");
        write!(stream, "GET {path} HTTP/1.1\r\n").expect("write request line");
        // TCP does not preserve write boundaries. Deliberately let the server
        // observe a fragmented request so closing with unread header bytes
        // cannot regress into a connection reset for real receivers.
        std::thread::sleep(Duration::from_millis(30));
        stream
            .write_all(b"Host: localhost\r\nConnection: close\r\n\r\n")
            .expect("write request headers");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        response
    }

    fn read_fcast_packet(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        let mut size = [0u8; 4];
        stream.read_exact(&mut size).expect("read FCast size");
        let size = u32::from_le_bytes(size) as usize;
        assert!(
            (1..=1024 * 1024).contains(&size),
            "invalid FCast packet size"
        );
        let mut packet = vec![0u8; size];
        stream.read_exact(&mut packet).expect("read FCast packet");
        (packet[0], packet[1..].to_vec())
    }

    fn write_fcast_packet(stream: &mut TcpStream, opcode: u8, body: &[u8]) {
        let body_len = u32::try_from(body.len()).unwrap_or(u32::MAX);
        let size = body_len.saturating_add(1);
        stream
            .write_all(&size.to_le_bytes())
            .expect("write FCast size");
        stream.write_all(&[opcode]).expect("write FCast opcode");
        stream.write_all(body).expect("write FCast body");
        stream.flush().expect("flush FCast packet");
    }

    #[test]
    fn fcast_receivers_load_hls_served_without_other_runtime_files() {
        // Phase `simulated_fcast_receiver_accepts_the_production_connect_and_hls_load`.
        {
            const VERSION_OPCODE: u8 = 11;
            const PLAY_OPCODE: u8 = 1;
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .expect("bind FCast simulator");
            let port = listener.local_addr().expect("simulator address").port();
            let receiver = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept FCast sender");
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .expect("receiver timeout");
                let (opcode, hello) = read_fcast_packet(&mut stream);
                assert_eq!(opcode, VERSION_OPCODE);
                let hello: serde_json::Value =
                    serde_json::from_slice(&hello).expect("version JSON");
                assert_eq!(hello["version"], 4);
                // Version 2 deliberately avoids TLS and gives the simulator a
                // compact, inspectable URL-load packet while still exercising the
                // SDK's production negotiation state machine.
                write_fcast_packet(&mut stream, VERSION_OPCODE, br#"{"version":2}"#);
                let (opcode, play) = read_fcast_packet(&mut stream);
                assert_eq!(opcode, PLAY_OPCODE);
                let play: serde_json::Value = serde_json::from_slice(&play).expect("play JSON");
                assert_eq!(play["container"], "application/vnd.apple.mpegurl");
                assert_eq!(
                    play["url"],
                    "http://127.0.0.1:43210/0123456789abcdef/live.m3u8"
                );
            });

            // Exercise the production connection helper: the returned owner must
            // keep its runtime alive while LOAD reaches the receiver.
            let cast = connect_cast_device(
                ProtocolType::FCast,
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                port,
                "FCast simulator".to_owned(),
            )
            .expect("production connection to simulator");
            cast.device
                .load(
                    LoadRequest::Url {
                        content_type: "application/vnd.apple.mpegurl".to_owned(),
                        url: "http://127.0.0.1:43210/0123456789abcdef/live.m3u8".to_owned(),
                        resume_position: None,
                        speed: None,
                        volume: None,
                        metadata: None,
                        request_headers: None,
                    },
                    None,
                )
                .expect("load projection URL");
            receiver.join().expect("FCast simulator assertions");
        }
        // Phase `media_server_serves_hls_without_exposing_other_runtime_files`.
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-http-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&root).expect("create fixture directory");
            fs::write(root.join("live.m3u8"), b"#EXTM3U\nsegment-00001.ts\n")
                .expect("write playlist");
            fs::write(root.join("segment-00001.ts"), b"transport-stream").expect("write segment");
            fs::write(root.join("session.pid"), b"secret").expect("write private fixture");
            let stop = Arc::new(AtomicBool::new(false));
            let token = "0123456789abcdef";
            let (port, thread) = start_http_server(
                root.clone(),
                stop.clone(),
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                token.to_owned(),
                Arc::new(MediaTelemetry::default()),
            )
            .expect("HTTP server");

            let playlist = String::from_utf8(request(port, &format!("/{token}/live.m3u8")))
                .expect("HTTP text");
            assert!(playlist.starts_with("HTTP/1.1 200 OK"));
            assert!(playlist.contains("Content-Type: application/vnd.apple.mpegurl"));
            assert!(playlist.ends_with("#EXTM3U\nsegment-00001.ts\n"));
            let segment = String::from_utf8(request(port, &format!("/{token}/segment-00001.ts")))
                .expect("HTTP text");
            assert!(segment.contains("Content-Type: video/mp2t"));
            assert!(segment.ends_with("transport-stream"));
            assert!(
                String::from_utf8(request(port, "/wrong/live.m3u8"))
                    .expect("HTTP text")
                    .starts_with("HTTP/1.1 400 Bad Request"),
                "a guessed playlist URL must not expose the stream"
            );
            let traversal = String::from_utf8(request(port, "/../session.pid")).expect("HTTP text");
            assert!(traversal.starts_with("HTTP/1.1 400 Bad Request"));
            let private = String::from_utf8(request(port, "/session.pid")).expect("HTTP text");
            assert!(private.starts_with("HTTP/1.1 400 Bad Request"));

            stop.store(true, Ordering::Release);
            wake_http_listener(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);
            thread.join().expect("join HTTP server");
            fs::remove_dir_all(root).expect("remove fixture directory");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn link_feedback_and_h264_muxing_produce_a_playable_bounded_stream() {
        // Phase `link_feedback_scales_continuously_without_crossing_the_display_ceiling`.
        {
            let ceiling = CaptureProfile::new(2560, 1440, 60, 15_000);
            let current = CaptureProfile::new(1920, 1080, 60, 10_000);
            let reduced = adapt_capture_profile(current, ceiling, 3_000_000, 300_000);
            assert_eq!((reduced.max_width, reduced.max_height), (1440, 810));
            assert!(reduced.bitrate_kbps < current.bitrate_kbps);

            let raised = adapt_capture_profile(current, ceiling, 30_000_000, 20_000);
            assert_eq!((raised.max_width, raised.max_height), (2560, 1440));
            assert!(raised.bitrate_kbps <= ceiling.bitrate_kbps);

            assert_eq!(
                adapt_capture_profile(ceiling, ceiling, 50_000_000, 20_000),
                ceiling
            );
        }
        // Phase `real_h264_frames_are_muxed_into_a_playable_hls_playlist`.
        {
            if Command::new("gst-inspect-1.0")
                .args(["h264parse", "hlssink2"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_err()
            {
                panic!("the test host is missing the packaged GStreamer inspection tool");
            }
            for element in ["h264parse", "hlssink2"] {
                assert!(
                    Command::new("gst-inspect-1.0")
                        .arg(element)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .expect("inspect GStreamer element")
                        .success(),
                    "required GStreamer element {element} is unavailable"
                );
            }

            let root = std::env::temp_dir().join(format!(
                "tilo-projector-hls-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let paths = SessionPaths::for_root(root.clone()).expect("session paths");
            paths.clear();
            // Large enough for every hardware encoder's minimum frame size.
            let size: u32 = 256;
            let mut pipeline =
                HlsPipeline::start(&paths, false).expect("start production HLS pipeline");
            // Projection receivers take H.264 only: hardware when the machine
            // has it, Cisco's openh264 through GStreamer otherwise.
            let mut encoder = tilo_callstream::VideoEncoder::with_options(
                tilo_callstream::StreamCodec::H264,
                size,
                size,
                30,
                1_000_000,
                30,
                true,
            )
            .expect("an H.264 encoder (hardware, or the openh264 GStreamer plugin)");
            let mut rgba = vec![0u8; (size * size * 4) as usize];
            for frame_index in 0..75u64 {
                let luma = u8::try_from(frame_index).unwrap_or(u8::MAX);
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&[luma, 96, 180, 255]);
                }
                for unit in encoder
                    .encode_at_ms(&rgba, frame_index * 33)
                    .expect("encode projection frame")
                {
                    pipeline
                        .push(&VideoPacket {
                            width: size,
                            height: size,
                            presentation_time_us: u64::try_from(unit.pts.as_micros())
                                .expect("a short test stream"),
                            keyframe: unit.keyframe,
                            annex_b: unit.data,
                        })
                        .expect("push projection frame");
                }
                std::thread::sleep(Duration::from_millis(34));
            }
            // This fixture is a finite producer. Encoding is asynchronous: the
            // final submitted frames can still be in flight after the last
            // encode call, including the keyframe that closes an HLS segment.
            // Drain those real units before waiting for the muxer's output.
            for unit in encoder.finish().expect("drain projection encoder") {
                pipeline
                    .push(&VideoPacket {
                        width: size,
                        height: size,
                        presentation_time_us: u64::try_from(unit.pts.as_micros())
                            .expect("a short test stream"),
                        keyframe: unit.keyframe,
                        annex_b: unit.data,
                    })
                    .expect("push final projection frame");
            }
            let deadline = tilo_test_env::hang_deadline();
            while !paths.playlist.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let playlist = fs::read_to_string(&paths.playlist).expect("generated HLS playlist");
            assert!(
                playlist.starts_with("#EXTM3U"),
                "unexpected playlist: {playlist}"
            );
            assert!(
                playlist.contains("segment-"),
                "playlist has no media segment"
            );
            assert!(
                fs::read_dir(&root)
                    .expect("read HLS output")
                    .flatten()
                    .any(|entry| entry.file_name().to_string_lossy().ends_with(".ts")),
                "HLS pipeline wrote no MPEG-TS segment"
            );
            drop(pipeline);
            fs::remove_dir_all(root).expect("remove HLS fixture directory");
        }
    }
}
