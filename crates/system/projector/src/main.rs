//! Tilo screen-projection protocol broker.
//!
//! This executable is intentionally separate from the compositor: discovery
//! touches the LAN, and protocol sessions will own receiver sockets for much
//! longer than the Win+P overlay is visible. The shell invokes the small JSON
//! command surface in `shell::shell::projection::SystemProjectionBackend`.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use fcast_sender_sdk::device::{
    ApplicationInfo, DeviceConnectionState, DeviceEventHandler, DeviceInfo, LoadRequest,
    MediaTrack, MediaTrackType, PlaybackState, ProtocolType, QueueState, ReceiverError, Source,
    TrackList,
};
use fcast_sender_sdk::{DeviceDiscovererEventHandler, url_format_ip_addr};
use getrandom::{SysRng, rand_core::UnwrapErr};
use mdns_sd::ServiceEvent;
use rand::Rng;
use tilo_projector::{
    CaptureProfile, DisplayResolutionLimit, DisplayStreamPolicy, ProjectionDevice, ProjectionMode,
    ProjectionProtocol, VideoPacket,
};

const DEFAULT_DISCOVERY_MS: u64 = 1_500;
const AIRPLAY_SERVICE: &str = "_airplay._tcp.local.";
const NM_DESTINATION: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_INTERFACE: &str = "org.freedesktop.NetworkManager";
const NM_DEVICE_INTERFACE: &str = "org.freedesktop.NetworkManager.Device";
const NM_P2P_INTERFACE: &str = "org.freedesktop.NetworkManager.Device.WifiP2P";
const NM_P2P_PEER_INTERFACE: &str = "org.freedesktop.NetworkManager.WifiP2PPeer";
const NM_DEVICE_TYPE_WIFI_P2P: u32 = 30;
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default)]
struct DiscoverySink {
    devices: Mutex<HashMap<String, DeviceInfo>>,
    changed: Mutex<Option<mpsc::Sender<()>>>,
}

impl DiscoverySink {
    fn with_signal(tx: mpsc::Sender<()>) -> Self {
        Self {
            devices: Mutex::new(HashMap::new()),
            changed: Mutex::new(Some(tx)),
        }
    }

    fn insert(&self, device: DeviceInfo) {
        let key = discovery_key(&device);
        let Ok(mut devices) = self.devices.lock() else {
            return;
        };
        devices.insert(key, device);
        drop(devices);
        let Ok(changed) = self.changed.lock() else {
            return;
        };
        if let Some(tx) = changed.as_ref() {
            let _ = tx.send(());
        }
    }
}

impl DeviceDiscovererEventHandler for DiscoverySink {
    fn device_available(&self, device_info: DeviceInfo) {
        self.insert(device_info);
    }

    fn device_removed(&self, device_name: String) {
        let Ok(mut devices) = self.devices.lock() else {
            return;
        };
        devices.retain(|_, device| device.name != device_name);
    }

    fn device_changed(&self, device_info: DeviceInfo) {
        self.insert(device_info);
    }
}

fn protocol(info: &DeviceInfo) -> ProjectionProtocol {
    match info.protocol {
        ProtocolType::FCast => ProjectionProtocol::FCast,
        ProtocolType::Chromecast => ProjectionProtocol::Chromecast,
    }
}

fn discovery_key(info: &DeviceInfo) -> String {
    let kind = protocol(info);
    let receiver_id = info
        .txt_records
        .get("id")
        .or_else(|| info.txt_records.get("deviceid"))
        .map_or(info.name.as_str(), String::as_str);
    format!("{}:{receiver_id}", kind.label())
}

fn into_projection_device(info: DeviceInfo) -> ProjectionDevice {
    let protocol = protocol(&info);
    let addresses: Vec<String> = info.addresses.iter().map(url_format_ip_addr).collect();
    let detail = addresses.first().map_or_else(
        || "Local network".to_owned(),
        |address| format!("{address}:{}", info.port),
    );
    ProjectionDevice {
        id: discovery_key(&info),
        name: info.name,
        protocol,
        detail,
        // A remote receiver is not a compositor output yet. Advertising only
        // mirror prevents the UI promising Extend/Only-there before the KMS
        // virtual-output path is attached.
        modes: vec![ProjectionMode::Mirror],
        addresses,
        port: info.port,
    }
}

fn airplay_projection_device(info: &mdns_sd::ResolvedService) -> ProjectionDevice {
    let name = info
        .get_fullname()
        .strip_suffix(&format!(".{AIRPLAY_SERVICE}"))
        .unwrap_or_else(|| info.get_fullname())
        .to_owned();
    let receiver_id = info
        .get_property_val_str("deviceid")
        .or_else(|| info.get_property_val_str("pi"))
        .unwrap_or_else(|| info.get_fullname());
    let mut addresses: Vec<String> = info
        .get_addresses()
        .iter()
        .map(|address| match address.to_ip_addr() {
            std::net::IpAddr::V4(address) => address.to_string(),
            std::net::IpAddr::V6(address) => format!("[{address}]"),
        })
        .collect();
    addresses.sort();
    let endpoint = addresses.first().map_or_else(
        || "Local network".to_owned(),
        |address| format!("{address}:{}", info.get_port()),
    );
    let detail = info
        .get_property_val_str("model")
        .map(|model| format!("{model} · {endpoint}"))
        .unwrap_or(endpoint);
    ProjectionDevice {
        id: format!("AirPlay:{receiver_id}"),
        name,
        protocol: ProjectionProtocol::AirPlay,
        detail,
        modes: vec![ProjectionMode::Mirror],
        addresses,
        port: info.get_port(),
    }
}

async fn discover_miracast_on_connection(
    connection: &zbus::Connection,
    timeout: Duration,
) -> zbus::Result<Vec<ProjectionDevice>> {
    use zbus::zvariant::OwnedObjectPath;

    let manager = zbus::Proxy::new(connection, NM_DESTINATION, NM_PATH, NM_INTERFACE).await?;
    let devices_reply = manager.call_method("GetDevices", &()).await?;
    let device_paths: Vec<OwnedObjectPath> = devices_reply.body().deserialize()?;
    let mut p2p_paths = Vec::new();
    for path in device_paths {
        let device = zbus::Proxy::new(
            connection,
            NM_DESTINATION,
            path.as_str(),
            NM_DEVICE_INTERFACE,
        )
        .await?;
        if device.get_property::<u32>("DeviceType").await? == NM_DEVICE_TYPE_WIFI_P2P {
            let p2p = zbus::Proxy::new(connection, NM_DESTINATION, path.as_str(), NM_P2P_INTERFACE)
                .await?;
            let options = HashMap::from([(
                "timeout",
                zbus::zvariant::Value::from(timeout.as_secs().clamp(1, 600) as i32),
            )]);
            p2p.call_method("StartFind", &options).await?;
            p2p_paths.push(path);
        }
    }

    tokio::time::sleep(timeout).await;
    let mut receivers = Vec::new();
    for path in p2p_paths {
        let p2p =
            zbus::Proxy::new(connection, NM_DESTINATION, path.as_str(), NM_P2P_INTERFACE).await?;
        let peers: Vec<OwnedObjectPath> = p2p.get_property("Peers").await?;
        for peer_path in peers {
            let peer = zbus::Proxy::new(
                connection,
                NM_DESTINATION,
                peer_path.as_str(),
                NM_P2P_PEER_INTERFACE,
            )
            .await?;
            let wfd_ies: Vec<u8> = peer.get_property("WfdIEs").await?;
            if wfd_ies.is_empty() {
                continue;
            }
            let name: String = peer.get_property("Name").await?;
            let address: String = peer.get_property("HwAddress").await?;
            let manufacturer: String = peer.get_property("Manufacturer").await?;
            let model: String = peer.get_property("Model").await?;
            let strength: u8 = peer.get_property("Strength").await?;
            let hardware = [manufacturer.as_str(), model.as_str()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let detail = if hardware.is_empty() {
                format!("Wi-Fi Direct · {strength}%")
            } else {
                format!("{hardware} · {strength}%")
            };
            receivers.push(ProjectionDevice {
                id: format!("Miracast:{address}"),
                name,
                protocol: ProjectionProtocol::Miracast,
                detail,
                modes: vec![ProjectionMode::Mirror],
                // Wi-Fi Direct has not formed a group yet; the peer MAC, not
                // an IP endpoint, identifies the receiver at this stage.
                addresses: vec![address],
                port: 0,
            });
        }
        let _ = p2p.call_method("StopFind", &()).await;
    }
    Ok(receivers)
}

fn discover_miracast(timeout: Duration) -> Vec<ProjectionDevice> {
    tilo_rt::block_on(async {
        let Ok(connection) = zbus::Connection::system().await else {
            return Vec::new();
        };
        discover_miracast_on_connection(&connection, timeout)
            .await
            .unwrap_or_default()
    })
    .unwrap_or_default()
}

fn discover(timeout: Duration) -> Result<Vec<ProjectionDevice>, String> {
    let miracast = std::thread::Builder::new()
        .name("tilo-miracast-discovery".to_owned())
        .spawn(move || discover_miracast(timeout))
        .map_err(|error| format!("cannot start Miracast discovery: {error}"))?;
    let context = fcast_sender_sdk::context::CastContext::new()
        .map_err(|error| format!("cannot initialise Cast discovery: {error}"))?;
    let (tx, rx) = mpsc::channel();
    let sink = Arc::new(DiscoverySink::with_signal(tx));
    context.start_discovery(sink.clone());
    let airplay_daemon = mdns_sd::ServiceDaemon::new()
        .map_err(|error| format!("cannot initialise AirPlay discovery: {error}"))?;
    let airplay_rx = airplay_daemon
        .browse(AIRPLAY_SERVICE)
        .map_err(|error| format!("cannot browse AirPlay receivers: {error}"))?;
    let mut airplay = HashMap::new();

    // Give both service browsers the full bounded collection window. The two
    // protocols have independent mDNS streams, so returning on the first quiet
    // gap can consistently lose whichever browser resolves second.
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "requested discovery timeout is too large".to_owned())?;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if matches!(
            rx.recv_timeout(Duration::from_millis(100).min(remaining)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ) {
            break;
        }
        while let Ok(event) = airplay_rx.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    let device = airplay_projection_device(&info);
                    airplay.insert(device.id.clone(), device);
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    airplay.retain(|_, device| !fullname.starts_with(&device.name));
                }
                _ => {}
            }
        }
    }
    let _ = airplay_daemon.shutdown();

    let mut devices: Vec<_> = sink
        .devices
        .lock()
        .map_err(|_| "projection discovery cache was poisoned".to_owned())?
        .values()
        .cloned()
        .map(into_projection_device)
        .collect();
    devices.extend(airplay.into_values());
    devices.extend(miracast.join().unwrap_or_default());
    devices.sort_by_key(|device| device.name.to_lowercase());
    Ok(devices)
}

#[derive(Clone)]
struct SessionPaths {
    root: PathBuf,
    video_socket: PathBuf,
    playlist: PathBuf,
    stop: PathBuf,
    ready: PathBuf,
    error: PathBuf,
    pid: PathBuf,
    profile: PathBuf,
}

impl SessionPaths {
    fn resolve() -> Result<Self, String> {
        let root = std::env::var_os("TILO_PROJECTOR_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| xdg_paths::xdg::runtime_dir().map(|dir| dir.join("tilo-projector")))
            .ok_or_else(|| "XDG_RUNTIME_DIR is unavailable".to_owned())?;
        Self::for_root(root)
    }

    fn for_root(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&root)
            .map_err(|error| format!("cannot create projection runtime directory: {error}"))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot secure projection runtime directory: {error}"))?;
        Ok(Self {
            video_socket: root.join("video.sock"),
            playlist: root.join("live.m3u8"),
            stop: root.join("stop"),
            ready: root.join("ready"),
            error: root.join("error"),
            pid: root.join("session.pid"),
            profile: root.join("capture-profile.json"),
            root,
        })
    }

    fn clear_markers(&self) {
        for path in [
            &self.video_socket,
            &self.playlist,
            &self.stop,
            &self.ready,
            &self.error,
            &self.pid,
            &self.profile,
        ] {
            let _ = fs::remove_file(path);
        }
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_segment =
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("segment-")
                                && Path::new(name).extension() == Some(OsStr::new("ts"))
                        });
                if is_segment {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}

struct HlsPipeline {
    child: Child,
    input: ChildStdin,
}

impl HlsPipeline {
    fn start(paths: &SessionPaths, audio: bool) -> Result<Self, String> {
        let executable =
            std::env::var("TILO_GST_LAUNCH").unwrap_or_else(|_| "gst-launch-1.0".to_owned());
        let segment = paths.root.join("segment-%05d.ts");
        let mut args = vec![
            "-q".to_owned(),
            "fdsrc".to_owned(),
            "fd=0".to_owned(),
            "do-timestamp=true".to_owned(),
            "!".to_owned(),
            "h264parse".to_owned(),
            "config-interval=-1".to_owned(),
            "!".to_owned(),
            "video/x-h264,stream-format=byte-stream,alignment=au".to_owned(),
            "!".to_owned(),
            "queue".to_owned(),
            "!".to_owned(),
            "mux.video".to_owned(),
            "hlssink2".to_owned(),
            "name=mux".to_owned(),
            "target-duration=1".to_owned(),
            "playlist-length=4".to_owned(),
            "max-files=6".to_owned(),
            format!("playlist-location={}", paths.playlist.display()),
            format!("location={}", segment.display()),
        ];
        if audio {
            args.extend([
                "pulsesrc".to_owned(),
                "device=@DEFAULT_MONITOR@".to_owned(),
                "do-timestamp=true".to_owned(),
                "!".to_owned(),
                "audioconvert".to_owned(),
                "!".to_owned(),
                "audioresample".to_owned(),
                "!".to_owned(),
                "audio/x-raw,rate=48000,channels=2".to_owned(),
                "!".to_owned(),
                "avenc_aac".to_owned(),
                "bitrate=192000".to_owned(),
                "!".to_owned(),
                "aacparse".to_owned(),
                "!".to_owned(),
                "queue".to_owned(),
                "!".to_owned(),
                "mux.audio".to_owned(),
            ]);
        }
        let mut child = Command::new(executable)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start HLS media pipeline: {error}"))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "HLS pipeline has no input".to_owned())?;
        Ok(Self { child, input })
    }

    fn push(&mut self, packet: &VideoPacket) -> Result<(), String> {
        self.input
            .write_all(&packet.annex_b)
            .map_err(|error| format!("HLS pipeline stopped accepting video: {error}"))
    }
}

impl Drop for HlsPipeline {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn video_ingest_loop(
    paths: &SessionPaths,
    stop: &Arc<AtomicBool>,
    audio: bool,
) -> Result<(), String> {
    let listener = UnixListener::bind(&paths.video_socket)
        .map_err(|error| format!("cannot bind compositor video socket: {error}"))?;
    fs::set_permissions(&paths.video_socket, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure compositor video socket: {error}"))?;
    let mut pipeline = HlsPipeline::start(paths, audio)?;
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) if stop.load(Ordering::Acquire) => {
                drop(stream);
                break;
            }
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                while !stop.load(Ordering::Acquire) {
                    match VideoPacket::read_from(&mut stream) {
                        Ok(packet) => pipeline.push(&packet)?,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("cannot accept compositor video: {error}")),
        }
    }
    Ok(())
}

struct MediaTelemetry {
    started: Instant,
    requests: AtomicU64,
    bytes: AtomicU64,
    service_us: AtomicU64,
    last_request_ms: AtomicU64,
}

impl MediaTelemetry {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            service_us: AtomicU64::new(0),
            last_request_ms: AtomicU64::new(0),
        }
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Default for MediaTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

fn serve_file(mut stream: TcpStream, root: &Path, token: &str, telemetry: &MediaTelemetry) {
    let request_started = Instant::now();
    let mut request = [0u8; 2048];
    let mut read = 0;
    loop {
        if read == request.len() {
            let _ = stream.write_all(
                b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let Some(remaining) = request.get_mut(read..) else {
            return;
        };
        let Ok(received) = stream.read(remaining) else {
            return;
        };
        if received == 0 {
            return;
        }
        let Some(total) = read.checked_add(received) else {
            return;
        };
        read = total;
        let Some(received_request) = request.get(..read) else {
            return;
        };
        if received_request
            .windows(4)
            .any(|bytes| bytes == b"\r\n\r\n")
        {
            break;
        }
    }
    let Some(bytes) = request.get(..read) else {
        return;
    };
    let first_line = String::from_utf8_lossy(bytes);
    let resource = first_line
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|path| path.strip_prefix('/'))
        .and_then(|path| path.strip_prefix(token))
        .and_then(|path| path.strip_prefix('/'))
        .filter(|path| {
            *path == "live.m3u8"
                || path.strip_prefix("segment-").is_some_and(|rest| {
                    rest.strip_suffix(".ts")
                        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
                })
        });
    let Some(resource) = resource else {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let path = root.join(resource);
    match fs::read(&path) {
        Ok(body) => {
            let content_type = if Path::new(resource).extension() == Some(OsStr::new("m3u8")) {
                "application/vnd.apple.mpegurl"
            } else {
                "video/mp2t"
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            telemetry.requests.fetch_add(1, Ordering::Relaxed);
            telemetry
                .bytes
                .fetch_add(body.len() as u64, Ordering::Relaxed);
            telemetry.service_us.fetch_add(
                u64::try_from(request_started.elapsed().as_micros()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
            telemetry
                .last_request_ms
                .store(telemetry.elapsed_millis(), Ordering::Release);
        }
        Err(_) => {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    }
}

fn start_http_server(
    root: PathBuf,
    stop: Arc<AtomicBool>,
    receiver: std::net::IpAddr,
    token: String,
    telemetry: Arc<MediaTelemetry>,
) -> Result<(u16, std::thread::JoinHandle<()>), String> {
    let bind_ip = if receiver.is_ipv6() {
        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    };
    let listener = TcpListener::bind(std::net::SocketAddr::new(bind_ip, 0))
        .map_err(|error| format!("cannot bind projection media server: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("cannot read projection media address: {error}"))?
        .port();
    let thread = std::thread::Builder::new()
        .name("tilo-proj-http".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(_) if stop.load(Ordering::Acquire) => break,
                    Ok((stream, peer)) if peer.ip() == receiver => {
                        serve_file(stream, &root, &token, &telemetry);
                    }
                    Ok((mut stream, _)) => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
        .map_err(|error| format!("cannot start projection media server: {error}"))?;
    Ok((port, thread))
}

fn wake_http_listener(receiver: std::net::IpAddr, port: u16) {
    let loopback = if receiver.is_ipv6() {
        std::net::SocketAddr::new(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), port)
    } else {
        std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port)
    };
    let _ = TcpStream::connect_timeout(&loopback, Duration::from_millis(200));
}

fn random_media_token() -> String {
    let mut bytes = [0u8; 24];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
    hex::encode(bytes)
}

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
) -> Result<(), String> {
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
        .map_err(|error| format!("receiver rejected the screen stream: {error}"))
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

fn parse_cast_protocol(value: &str) -> Result<ProtocolType, String> {
    match value {
        "f-cast" | "fcast" => Ok(ProtocolType::FCast),
        "chromecast" => Ok(ProtocolType::Chromecast),
        other => Err(format!(
            "{other} transport is not implemented by the HLS session"
        )),
    }
}

fn parse_stream_policy(args: &[String]) -> Result<DisplayStreamPolicy, String> {
    let resolution_limit = match value_after(args, "--resolution-limit")
        .as_deref()
        .unwrap_or("native")
    {
        "native" => DisplayResolutionLimit::Native,
        "p2160" => DisplayResolutionLimit::P2160,
        "p1440" => DisplayResolutionLimit::P1440,
        "p1080" => DisplayResolutionLimit::P1080,
        "p720" => DisplayResolutionLimit::P720,
        value => return Err(format!("invalid display resolution limit {value}")),
    };
    let max_fps = value_after(args, "--max-fps")
        .as_deref()
        .unwrap_or("60")
        .parse::<u32>()
        .map_err(|_| "invalid --max-fps".to_owned())?;
    if !(24..=120).contains(&max_fps) {
        return Err("--max-fps must be between 24 and 120".to_owned());
    }
    Ok(DisplayStreamPolicy {
        adaptive: !args.iter().any(|arg| arg == "--fixed"),
        resolution_limit,
        max_fps,
    })
}

fn publish_capture_profile(paths: &SessionPaths, profile: CaptureProfile) -> Result<(), String> {
    let bytes = serde_json::to_vec(&profile)
        .map_err(|error| format!("cannot encode capture profile: {error}"))?;
    fs::write(&paths.profile, bytes)
        .map_err(|error| format!("cannot publish capture profile: {error}"))
}

fn run_airplay_session(args: &[String]) -> Result<(), String> {
    let address = value_after(args, "--address").ok_or("missing --address")?;
    let port = value_after(args, "--port")
        .ok_or("missing --port")?
        .parse::<u16>()
        .map_err(|_| "invalid --port".to_owned())?;
    let audio = args.iter().any(|arg| arg == "--audio");
    let stream_policy = parse_stream_policy(args)?;
    let executable = std::env::var("TILO_DOUBLETAKE")
        .unwrap_or_else(|_| "/usr/libexec/tilo-doubletake".to_owned());
    let paths = SessionPaths::resolve()?;
    paths.clear_markers();
    fs::write(&paths.pid, std::process::id().to_string())
        .map_err(|error| format!("cannot record AirPlay session: {error}"))?;
    let credentials = airplay_credentials_path()?;
    let command_args = airplay_command_args(&address, port, audio, stream_policy, &credentials);
    let pairing_credential = std::env::var("TILO_PROJECTION_CREDENTIAL")
        .ok()
        .filter(|value| !value.is_empty());
    run_airplay_process(
        &executable,
        &command_args,
        &paths,
        pairing_credential.as_deref(),
    )
}

fn airplay_credentials_path() -> Result<PathBuf, String> {
    airplay_credentials_path_from(
        std::env::var_os("TILO_PROJECTOR_STATE_DIR"),
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn airplay_credentials_path_from(
    override_root: Option<std::ffi::OsString>,
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    let state_root = override_root
        .map(PathBuf::from)
        .or_else(|| {
            xdg_paths::xdg::Base::State
                .resolve_from(xdg_state_home, home)
                .map(|dir| dir.join("tilo-projector"))
        })
        .ok_or_else(|| "cannot resolve persistent AirPlay pairing storage".to_owned())?;
    fs::create_dir_all(&state_root)
        .map_err(|error| format!("cannot create AirPlay pairing storage: {error}"))?;
    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure AirPlay pairing storage: {error}"))?;
    Ok(state_root.join("airplay-credentials.json"))
}

fn airplay_command_args(
    address: &str,
    port: u16,
    audio: bool,
    stream_policy: DisplayStreamPolicy,
    credentials: &Path,
) -> Vec<String> {
    let profile = stream_policy.ceiling(None);
    let mut args = vec![
        "-target".to_owned(),
        address.trim_matches(['[', ']']).to_owned(),
        "-port".to_owned(),
        port.to_string(),
        "-fps".to_owned(),
        profile.fps.to_string(),
        "-bitrate".to_owned(),
        profile.bitrate_kbps.to_string(),
        "-hwaccel".to_owned(),
        "openh264".to_owned(),
        "-creds".to_owned(),
        credentials.display().to_string(),
    ];
    if !audio {
        args.push("-no-audio".to_owned());
    }
    args
}

fn run_airplay_process(
    executable: &str,
    args: &[String],
    paths: &SessionPaths,
    pairing_credential: Option<&str>,
) -> Result<(), String> {
    run_external_process(
        executable,
        args,
        paths,
        &["mirror session ready"],
        "AirPlay",
        pairing_credential.map(|value| ("DOUBLETAKE_CODE", value)),
    )
}

fn run_miracast_session(args: &[String]) -> Result<(), String> {
    let peer = value_after(args, "--address").ok_or("missing --address")?;
    let audio = args.iter().any(|arg| arg == "--audio");
    let stream_policy = parse_stream_policy(args)?;
    let executable =
        std::env::var("TILO_FLUXCAST").unwrap_or_else(|_| "/usr/libexec/tilo-fluxcast".to_owned());
    let paths = SessionPaths::resolve()?;
    paths.clear_markers();
    fs::write(&paths.pid, std::process::id().to_string())
        .map_err(|error| format!("cannot record Miracast session: {error}"))?;
    let command_args = miracast_command_args(peer, audio, stream_policy);
    run_external_process(
        &executable,
        &command_args,
        &paths,
        &["PLAY accepted; media stream started."],
        "Miracast",
        None,
    )
}

fn miracast_command_args(
    peer: String,
    audio: bool,
    stream_policy: DisplayStreamPolicy,
) -> Vec<String> {
    let profile = stream_policy.ceiling(None);
    let mut args = vec![
        "--protocol".to_owned(),
        "wfd".to_owned(),
        "--wfd-peer".to_owned(),
        peer,
        "--fps".to_owned(),
        profile.fps.to_string(),
        "--bitrate".to_owned(),
        format!("{}k", profile.bitrate_kbps),
        "--wfd-capture-backend".to_owned(),
        "portal".to_owned(),
        "--wfd-media-pipeline".to_owned(),
        "ffmpeg".to_owned(),
    ];
    if let Some((width, height)) = stream_policy.resolution_limit.dimensions() {
        args.extend(["--output-res".to_owned(), format!("{width}x{height}")]);
    }
    if !audio {
        args.push("--wfd-no-audio".to_owned());
    }
    args
}

fn run_external_process(
    executable: &str,
    args: &[String],
    paths: &SessionPaths,
    ready_messages: &[&str],
    transport_name: &str,
    credential_env: Option<(&str, &str)>,
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command
        .args(args)
        // Keep the sender and its GStreamer capture children in one group so
        // Disconnect cannot strand a camera/audio/capture process.
        .process_group(0)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((name, value)) = credential_env {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start {transport_name} sender: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{transport_name} sender has no status stream"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{transport_name} sender has no diagnostic stream"))?;
    let (line_tx, line_rx) = mpsc::channel();
    let stdout_tx = line_tx.clone();
    let stdout_reader = std::thread::Builder::new()
        .name("tilo-projection-status-out".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if stdout_tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| format!("cannot monitor {transport_name} sender output: {error}"))?;
    let stderr_reader = std::thread::Builder::new()
        .name("tilo-projection-status-err".to_owned())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        })
        .map_err(|error| format!("cannot monitor {transport_name} diagnostics: {error}"))?;
    let mut diagnostics = Vec::new();
    let mut ready = false;
    loop {
        while let Ok(line) = line_rx.try_recv() {
            if ready_messages.iter().any(|message| line.contains(message)) {
                fs::write(&paths.ready, b"ready").map_err(|error| {
                    format!("cannot publish {transport_name} readiness: {error}")
                })?;
                ready = true;
            }
            diagnostics.push(line);
            if diagnostics.len() > 8 {
                diagnostics.remove(0);
            }
        }
        if paths.stop.exists() {
            bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            paths.clear_markers();
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("cannot inspect AirPlay sender: {error}"))?
        {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            while let Ok(line) = line_rx.try_recv() {
                diagnostics.push(line);
            }
            let detail = diagnostics.join("\n");
            return Err(if detail.is_empty() {
                format!("{transport_name} sender exited unexpectedly ({status})")
            } else if ready {
                format!("{transport_name} stream ended ({status}): {detail}")
            } else {
                detail
            });
        }
        std::thread::sleep(SESSION_POLL_INTERVAL);
    }
}

/// `(device, connected session, receiver-message channel)` — the result of
/// [`connect_cast_device`].
type CastConnection = (
    Arc<dyn fcast_sender_sdk::device::CastingDevice>,
    ConnectedSession,
    mpsc::Receiver<String>,
);

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
) -> Result<CastConnection, String> {
    let context = fcast_sender_sdk::context::CastContext::new()
        .map_err(|error| format!("cannot initialise Cast session: {error}"))?;
    let info = DeviceInfo {
        name,
        protocol,
        addresses: vec![fcast_sender_sdk::IpAddr::from(address)],
        port,
        txt_records: HashMap::new(),
    };
    let device = context.create_device_from_info(info);
    let (connected_tx, connected_rx) = mpsc::channel();
    let (fault_tx, fault_rx) = mpsc::channel();
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
        .map_err(|error| format!("cannot start receiver connection: {error}"))?;
    let connected = connected_rx
        .recv_timeout(Duration::from_secs(8))
        .map_err(|_| "receiver did not accept the connection in time".to_owned())??;
    Ok((device, connected, fault_rx))
}

/// The pieces of a running cast session the adaptive feedback loop needs to
/// read but does not own.
struct CastFeedbackContext<'a> {
    paths: &'a SessionPaths,
    device: &'a Arc<dyn fcast_sender_sdk::device::CastingDevice>,
    url: &'a str,
    telemetry: &'a MediaTelemetry,
    fault_rx: &'a mpsc::Receiver<String>,
}

/// Watches receiver telemetry until `ctx.paths.stop` appears, reloading the
/// receiver after a stall and (when `adaptive`) publishing a new capture
/// profile every 3 seconds.
///
/// # Errors
///
/// Returns an error if the receiver stops fetching the stream after three
/// recovery attempts, if reloading the stream on the receiver fails, or if
/// publishing a new capture profile fails.
fn run_adaptive_feedback_loop(
    ctx: &CastFeedbackContext<'_>,
    adaptive: bool,
    maximum_profile: CaptureProfile,
    initial_profile: CaptureProfile,
) -> Result<(), String> {
    let mut last_feedback = Instant::now();
    let mut last_bytes = 0;
    let mut last_feedback_requests = 0;
    let mut last_service_us = 0;
    let mut last_seen_requests = 0;
    let mut current_profile = initial_profile;
    let mut recovery_attempts = 0u8;
    while !ctx.paths.stop.exists() {
        std::thread::sleep(Duration::from_millis(100));
        let requests = ctx.telemetry.requests.load(Ordering::Acquire);
        let now_ms = ctx.telemetry.elapsed_millis();
        let last_request_ms = ctx.telemetry.last_request_ms.load(Ordering::Acquire);
        let receiver_stalled = requests > 0 && now_ms.saturating_sub(last_request_ms) > 6_000;
        let receiver_fault = ctx.fault_rx.try_recv().is_ok();
        if receiver_stalled || receiver_fault {
            if recovery_attempts >= 3 {
                return Err(
                    "receiver stopped fetching the stream after three recovery attempts".to_owned(),
                );
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
                publish_capture_profile(ctx.paths, next)?;
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

fn run_cast_session(args: &[String]) -> Result<(), String> {
    let protocol_arg = value_after(args, "--protocol").ok_or("missing --protocol")?;
    let protocol = parse_cast_protocol(&protocol_arg)?;
    let address_arg = value_after(args, "--address").ok_or("missing --address")?;
    let address: std::net::IpAddr = address_arg
        .trim_matches(['[', ']'])
        .parse()
        .map_err(|error| format!("invalid receiver address: {error}"))?;
    let port = value_after(args, "--port")
        .ok_or("missing --port")?
        .parse::<u16>()
        .map_err(|_| "invalid --port".to_owned())?;
    let name = value_after(args, "--device").unwrap_or_else(|| "Wireless display".to_owned());
    let audio = args.iter().any(|arg| arg == "--audio");
    let stream_policy = parse_stream_policy(args)?;
    let paths = SessionPaths::resolve()?;
    paths.clear_markers();
    let mut maximum_profile = stream_policy.ceiling(None);
    let initial_profile = initial_capture_profile(stream_policy, maximum_profile);
    publish_capture_profile(&paths, initial_profile)?;
    fs::write(&paths.pid, std::process::id().to_string())
        .map_err(|error| format!("cannot record projection session: {error}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let video_paths = paths.clone();
    let video_stop = stop.clone();
    let video_thread = std::thread::Builder::new()
        .name("tilo-proj-video".to_owned())
        .spawn(move || video_ingest_loop(&video_paths, &video_stop, audio))
        .map_err(|error| format!("cannot start projection video ingest: {error}"))?;
    let media_token = random_media_token();
    let telemetry = Arc::new(MediaTelemetry::new());
    let (http_port, http_thread) = start_http_server(
        paths.root.clone(),
        stop.clone(),
        address,
        media_token.clone(),
        telemetry.clone(),
    )?;

    let (device, connected, fault_rx) = connect_cast_device(protocol, address, port, name)?;
    maximum_profile = stream_policy.ceiling(connected.receiver_resolution);
    let local_addr = connected.local_addr;
    let initial_profile = initial_capture_profile(stream_policy, maximum_profile);
    publish_capture_profile(&paths, initial_profile)?;

    let playlist_deadline = Instant::now()
        .checked_add(Duration::from_secs(8))
        .ok_or_else(|| "system clock cannot represent the playlist deadline".to_owned())?;
    while !paths.playlist.exists() && Instant::now() < playlist_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !paths.playlist.exists() {
        return Err("the compositor did not deliver video frames in time".to_owned());
    }
    let url = format!(
        "http://{}:{http_port}/{media_token}/live.m3u8",
        fcast_sender_sdk::url_format_ip_addr(&local_addr)
    );
    cast_load(&device, &url)?;
    fs::write(&paths.ready, b"ready")
        .map_err(|error| format!("cannot publish projection readiness: {error}"))?;

    let feedback_ctx = CastFeedbackContext {
        paths: &paths,
        device: &device,
        url: &url,
        telemetry: &telemetry,
        fault_rx: &fault_rx,
    };
    run_adaptive_feedback_loop(
        &feedback_ctx,
        stream_policy.adaptive,
        maximum_profile,
        initial_profile,
    )?;

    let _ = device.stop_playback();
    let _ = device.disconnect();
    stop.store(true, Ordering::Release);
    // Wake workers that are waiting in accept.
    let _ = std::os::unix::net::UnixStream::connect(&paths.video_socket);
    wake_http_listener(address, http_port);
    let _ = video_thread.join();
    let _ = http_thread.join();
    paths.clear_markers();
    Ok(())
}

fn connect_command(args: &[String]) -> Result<(), String> {
    let protocol = value_after(args, "--protocol").ok_or("missing --protocol")?;
    let session_command = match protocol.as_str() {
        "air-play" | "airplay" => "_airplay_session",
        "miracast" => "_miracast_session",
        _ => {
            parse_cast_protocol(&protocol)?;
            "_session"
        }
    };
    let paths = SessionPaths::resolve()?;
    paths.clear_markers();
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot resolve tilo-projector executable: {error}"))?;
    let mut child_args = vec![session_command.to_owned()];
    child_args.extend_from_slice(args.get(1..).unwrap_or(&[]));
    let mut child = Command::new(executable)
        .args(child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start projection session: {error}"))?;
    let deadline = Instant::now()
        .checked_add(SESSION_READY_TIMEOUT)
        .ok_or_else(|| "system clock cannot represent the session-ready deadline".to_owned())?;
    while Instant::now() < deadline {
        if paths.ready.exists() {
            return Ok(());
        }
        if let Ok(error) = fs::read_to_string(&paths.error) {
            return Err(error);
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("cannot inspect projection session: {error}"))?
        {
            return Err(format!(
                "projection session exited before it was ready ({status})"
            ));
        }
        std::thread::sleep(SESSION_POLL_INTERVAL);
    }
    // Give the worker a chance to terminate its transport child as well. This
    // matters for AirPlay, whose sender is a separate statically-linked helper.
    let _ = fs::write(&paths.stop, b"stop");
    if let Some(cleanup_deadline) = Instant::now().checked_add(Duration::from_secs(2)) {
        while Instant::now() < cleanup_deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(SESSION_POLL_INTERVAL);
        }
    }
    let _ = child.kill();
    Err("projection session did not become ready in time".to_owned())
}

fn disconnect_command() -> Result<(), String> {
    let paths = SessionPaths::resolve()?;
    if !paths.pid.exists() {
        paths.clear_markers();
        return Ok(());
    }
    fs::write(&paths.stop, b"stop")
        .map_err(|error| format!("cannot stop projection session: {error}"))?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(3))
        .ok_or_else(|| "system clock cannot represent the disconnect deadline".to_owned())?;
    while paths.pid.exists() && Instant::now() < deadline {
        std::thread::sleep(SESSION_POLL_INTERVAL);
    }
    if paths.pid.exists() {
        return Err("projection session did not stop cleanly".to_owned());
    }
    Ok(())
}

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find_map(|pair| match pair {
        [name, value] if name == flag => Some(value.clone()),
        _ => None,
    })
}

fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("discover") => {
            let timeout_ms = value_after(args, "--timeout-ms")
                .map(|raw| {
                    raw.parse::<u64>()
                        .map_err(|_| "invalid --timeout-ms".to_owned())
                })
                .transpose()?
                .unwrap_or(DEFAULT_DISCOVERY_MS);
            let devices = discover(Duration::from_millis(timeout_ms))?;
            println!(
                "{}",
                serde_json::to_string(&devices)
                    .map_err(|error| format!("cannot encode discovery result: {error}"))?
            );
            Ok(())
        }
        Some("connect") => connect_command(args),
        Some("disconnect") => disconnect_command(),
        Some("_session") => match run_cast_session(args) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Ok(paths) = SessionPaths::resolve() {
                    let _ = fs::write(&paths.error, &error);
                    let _ = fs::remove_file(&paths.pid);
                }
                Err(error)
            }
        },
        Some("_airplay_session") => match run_airplay_session(args) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Ok(paths) = SessionPaths::resolve() {
                    let _ = fs::write(&paths.error, &error);
                    let _ = fs::remove_file(&paths.pid);
                }
                Err(error)
            }
        },
        Some("_miracast_session") => match run_miracast_session(args) {
            Ok(()) => Ok(()),
            Err(error) => {
                if let Ok(paths) = SessionPaths::resolve() {
                    let _ = fs::write(&paths.error, &error);
                    let _ = fs::remove_file(&paths.pid);
                }
                Err(error)
            }
        },
        _ => Err(
            "usage: tilo-projector discover --json [--timeout-ms N] | connect ... | disconnect ..."
                .to_owned(),
        ),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&args) {
        eprintln!("tilo-projector: {error}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::used_underscore_binding,
        reason = "zbus::interface's generated dispatcher binds start_find's _options parameter \
                  by name in dispatch code that lands in this module but outside the annotated \
                  impl, so neither an impl- nor fn-level allow reaches it; the mock ignores it"
    )]
    use super::*;
    use fcast_sender_sdk::IpAddr;
    use mdns_sd::{ServiceDaemon, ServiceInfo};
    use zbus::connection::Builder;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";
    const TEST_P2P_PATH: &str = "/org/freedesktop/NetworkManager/Devices/7";
    const TEST_PEER_PATH: &str = "/org/freedesktop/NetworkManager/WifiP2PPeer/9";

    struct SimulatedNetworkManager;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl SimulatedNetworkManager {
        fn get_devices(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(TEST_P2P_PATH).expect("device path")]
        }
    }

    struct SimulatedDevice;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl SimulatedDevice {
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            NM_DEVICE_TYPE_WIFI_P2P
        }
    }

    struct SimulatedP2pDevice;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.WifiP2P")]
    impl SimulatedP2pDevice {
        fn start_find(&self, _options: HashMap<String, OwnedValue>) {}

        fn stop_find(&self) {}

        #[zbus(property)]
        fn peers(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from(TEST_PEER_PATH).expect("peer path")]
        }
    }

    struct SimulatedMiracastPeer;

    #[allow(
        clippy::unused_self,
        reason = "zbus::interface dispatches by instance method, even for a stateless mock reply"
    )]
    #[zbus::interface(name = "org.freedesktop.NetworkManager.WifiP2PPeer")]
    impl SimulatedMiracastPeer {
        #[zbus(property)]
        fn name(&self) -> &'static str {
            "Conference display"
        }

        #[zbus(property)]
        fn hw_address(&self) -> &'static str {
            "02:00:00:00:00:09"
        }

        #[zbus(property)]
        fn manufacturer(&self) -> &'static str {
            "Tilo Labs"
        }

        #[zbus(property)]
        fn model(&self) -> &'static str {
            "WFD Simulator"
        }

        #[zbus(property)]
        fn strength(&self) -> u8 {
            87
        }

        #[zbus(property, name = "WfdIEs")]
        fn wfd_ies(&self) -> Vec<u8> {
            // Presence is the NetworkManager contract for a Wi-Fi Display
            // capable P2P peer. Parsing the WFD subelements belongs to the
            // RTSP session driver, not the picker.
            vec![0x00, 0x00, 0x06, 0x00, 0x01, 0x1c, 0x44, 0x00, 0x00]
        }
    }

    async fn simulated_network_manager() -> (zbus::Connection, zbus::Connection) {
        let (server_socket, client_socket) = tokio::net::UnixStream::pair().expect("socket pair");
        let server = Builder::unix_stream(server_socket)
            .server(TEST_GUID)
            .expect("server GUID")
            .p2p()
            .serve_at(NM_PATH, SimulatedNetworkManager)
            .expect("manager service")
            .serve_at(TEST_P2P_PATH, SimulatedDevice)
            .expect("base device service")
            .serve_at(TEST_P2P_PATH, SimulatedP2pDevice)
            .expect("P2P device service")
            .serve_at(TEST_PEER_PATH, SimulatedMiracastPeer)
            .expect("peer service");
        let client = Builder::unix_stream(client_socket).p2p();
        futures_util::try_join!(server.build(), client.build()).expect("D-Bus simulator pair")
    }

    fn info(protocol: ProtocolType) -> DeviceInfo {
        DeviceInfo {
            name: "Studio TV".to_owned(),
            protocol,
            addresses: vec![IpAddr::v4(192, 0, 2, 40)],
            port: 46899,
            txt_records: HashMap::from([("id".to_owned(), "receiver-42".to_owned())]),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn discovery_records_share_the_broker_contract_and_stay_bounded() {
        // Phase `fcast_discovery_record_preserves_endpoint_and_identity`.
        {
            let device = into_projection_device(info(ProtocolType::FCast));
            assert_eq!(device.protocol, ProjectionProtocol::FCast);
            assert_eq!(device.id, "FCast:receiver-42");
            assert_eq!(device.addresses, ["192.0.2.40"]);
            assert_eq!(device.port, 46899);
            assert_eq!(device.modes, [ProjectionMode::Mirror]);
        }
        // Phase `chromecast_discovery_record_uses_the_same_broker_contract`.
        {
            let mut cast = info(ProtocolType::Chromecast);
            cast.port = 8009;
            let device = into_projection_device(cast);
            assert_eq!(device.protocol, ProjectionProtocol::Chromecast);
            assert_eq!(device.id, "Google Cast:receiver-42");
            assert_eq!(device.port, 8009);
        }
        // Phase `zero_length_discovery_is_bounded_and_json_serialisable`.
        {
            let devices = discover(Duration::ZERO).expect("empty bounded discovery");
            serde_json::to_string(&devices).expect("projection discovery JSON");
        }
        // Phase `local_mdns_receivers_exercise_three_real_discovery_browsers`.
        {
            let daemon = ServiceDaemon::new().expect("mDNS simulator daemon");
            let suffix = std::process::id();
            let fcast_name = format!("Tilo FCast simulator {suffix}");
            let cast_name = format!("Tilo Cast simulator {suffix}");
            let airplay_name = format!("Tilo AirPlay simulator {suffix}");

            let fcast = ServiceInfo::new(
                "_fcast._tcp.local.",
                &fcast_name,
                &format!("tilo-fcast-{suffix}.local."),
                "",
                46899,
                HashMap::from([("id".to_owned(), format!("fcast-{suffix}"))]),
            )
            .expect("FCast simulator record")
            .enable_addr_auto();
            let cast = ServiceInfo::new(
                "_googlecast._tcp.local.",
                &format!("tilo-cast-{suffix}"),
                &format!("tilo-cast-{suffix}.local."),
                "",
                8009,
                HashMap::from([
                    ("id".to_owned(), format!("cast-{suffix}")),
                    ("fn".to_owned(), cast_name.clone()),
                ]),
            )
            .expect("Google Cast simulator record")
            .enable_addr_auto();
            let airplay = ServiceInfo::new(
                AIRPLAY_SERVICE,
                &airplay_name,
                &format!("tilo-airplay-{suffix}.local."),
                "",
                7000,
                HashMap::from([
                    ("deviceid".to_owned(), format!("airplay-{suffix}")),
                    ("model".to_owned(), "AudioAccessory6,1".to_owned()),
                    ("features".to_owned(), "0x5A7FFFF7,0x1E".to_owned()),
                ]),
            )
            .expect("AirPlay simulator record")
            .enable_addr_auto();
            daemon.register(fcast).expect("register FCast simulator");
            daemon.register(cast).expect("register Cast simulator");
            daemon
                .register(airplay)
                .expect("register AirPlay simulator");
            std::thread::sleep(Duration::from_millis(300));

            let devices = discover(Duration::from_secs(3)).expect("discover simulated receivers");
            let _ = daemon.shutdown();

            assert!(
                devices.iter().any(|device| {
                    device.name == fcast_name
                        && device.protocol == ProjectionProtocol::FCast
                        && device.port == 46899
                }),
                "FCast simulator missing from {devices:?}"
            );
            assert!(
                devices.iter().any(|device| {
                    device.name == cast_name
                        && device.protocol == ProjectionProtocol::Chromecast
                        && device.port == 8009
                }),
                "Google Cast simulator missing from {devices:?}"
            );
            assert!(
                devices.iter().any(|device| {
                    device.name == airplay_name
                        && device.protocol == ProjectionProtocol::AirPlay
                        && device.port == 7000
                }),
                "AirPlay simulator missing from {devices:?}"
            );
        }
    }

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
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
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
                    .set_read_timeout(Some(Duration::from_secs(5)))
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

            let context = fcast_sender_sdk::context::CastContext::new().expect("Cast context");
            let device = context.create_device_from_info(DeviceInfo {
                name: "FCast simulator".to_owned(),
                protocol: ProtocolType::FCast,
                addresses: vec![IpAddr::v4(127, 0, 0, 1)],
                port,
                txt_records: HashMap::new(),
            });
            let (connected_tx, connected_rx) = mpsc::channel();
            let (faults, _fault_rx) = mpsc::channel();
            device
                .connect(
                    Some(ApplicationInfo {
                        name: "tilo-projector".to_owned(),
                        version: env!("CARGO_PKG_VERSION").to_owned(),
                        display_name: "Tilo Screen Projection".to_owned(),
                    }),
                    Arc::new(SessionEvents {
                        connected: Mutex::new(Some(connected_tx)),
                        faults,
                    }),
                    0,
                )
                .expect("connect to simulator");
            connected_rx
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("connected event")
                .expect("successful connection");
            device
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
            paths.clear_markers();
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn airplay_miracast_and_external_senders_run_without_hardware() {
        // Phase `airplay_process_readiness_and_disconnect_are_hardware_free`.
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-airplay-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let paths = SessionPaths::for_root(root.clone()).expect("session paths");
            paths.clear_markers();
            let sender = root.join("airplay-sender-simulator");
            fs::write(
                &sender,
                b"#!/bin/sh\n[ \"$DOUBLETAKE_CODE\" = \"4821\" ] || exit 7\ntrap 'exit 0' TERM INT\necho 'screen capture started' >&2\necho 'mirror session ready (data port: 7001)' >&2\nwhile :; do sleep 1; done\n",
            )
            .expect("write AirPlay simulator");
            fs::set_permissions(&sender, fs::Permissions::from_mode(0o700))
                .expect("make simulator executable");
            let worker_paths = paths.clone();
            let sender_path = sender.display().to_string();
            let session = std::thread::spawn(move || {
                run_airplay_process(&sender_path, &[], &worker_paths, Some("4821"))
            });
            let deadline = tilo_test_env::hang_deadline();
            while !paths.ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(paths.ready.exists(), "AirPlay readiness was not published");
            fs::write(&paths.stop, b"stop").expect("request AirPlay disconnect");
            session
                .join()
                .expect("join AirPlay session")
                .expect("clean AirPlay disconnect");
            assert!(!paths.ready.exists());
            assert!(!paths.stop.exists());
            fs::remove_dir_all(root).expect("remove AirPlay fixture directory");
        }
        // Phase `airplay_pairing_credentials_use_persistent_private_storage`.
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-state-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let path =
                airplay_credentials_path_from(Some(root.clone().into_os_string()), None, None)
                    .expect("persistent credential path");
            assert_eq!(path, root.join("airplay-credentials.json"));
            assert_eq!(
                fs::metadata(&root)
                    .expect("state metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            fs::remove_dir_all(root).expect("remove state fixture");
        }
        // Phase `miracast_process_readiness_and_disconnect_are_hardware_free`.
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-miracast-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let paths = SessionPaths::for_root(root.clone()).expect("session paths");
            paths.clear_markers();
            let sender = root.join("miracast-sender-simulator");
            fs::write(
                &sender,
                b"#!/bin/sh\ntrap 'exit 0' TERM INT\necho 'PLAY accepted; media stream started.'\nwhile :; do sleep 1; done\n",
            )
            .expect("write Miracast simulator");
            fs::set_permissions(&sender, fs::Permissions::from_mode(0o700))
                .expect("make simulator executable");
            let worker_paths = paths.clone();
            let sender_path = sender.display().to_string();
            let session = std::thread::spawn(move || {
                run_external_process(
                    &sender_path,
                    &[],
                    &worker_paths,
                    &["PLAY accepted; media stream started."],
                    "Miracast",
                    None,
                )
            });
            let deadline = tilo_test_env::hang_deadline();
            while !paths.ready.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(paths.ready.exists(), "Miracast readiness was not published");
            fs::write(&paths.stop, b"stop").expect("request Miracast disconnect");
            session
                .join()
                .expect("join Miracast session")
                .expect("clean Miracast disconnect");
            assert!(!paths.ready.exists());
            assert!(!paths.stop.exists());
            fs::remove_dir_all(root).expect("remove Miracast fixture directory");
        }
        // Phase `external_sender_arguments_select_portal_capture_audio_and_exact_receiver`.
        {
            let airplay = airplay_command_args(
                "[2001:db8::7]",
                7000,
                false,
                DisplayStreamPolicy {
                    adaptive: true,
                    resolution_limit: DisplayResolutionLimit::P2160,
                    max_fps: 60,
                },
                Path::new("/state/pairing.json"),
            );
            assert!(airplay.windows(2).any(|a| a == ["-target", "2001:db8::7"]));
            assert!(airplay.windows(2).any(|a| a == ["-port", "7000"]));
            assert!(airplay.windows(2).any(|a| a == ["-hwaccel", "openh264"]));
            assert!(airplay.iter().any(|a| a == "-no-audio"));

            let miracast = miracast_command_args(
                "02:00:00:00:00:09".to_owned(),
                true,
                DisplayStreamPolicy {
                    adaptive: true,
                    resolution_limit: DisplayResolutionLimit::P720,
                    max_fps: 60,
                },
            );
            assert!(
                miracast
                    .windows(2)
                    .any(|a| a == ["--wfd-peer", "02:00:00:00:00:09"])
            );
            assert!(
                miracast
                    .windows(2)
                    .any(|a| a == ["--wfd-capture-backend", "portal"])
            );
            assert!(
                miracast
                    .windows(2)
                    .any(|a| a == ["--wfd-media-pipeline", "ffmpeg"])
            );
            assert!(miracast.windows(2).any(|a| a == ["--fps", "60"]));
            assert!(!miracast.iter().any(|a| a == "--wfd-no-audio"));
        }
    }

    #[tokio::test]
    async fn simulated_networkmanager_peer_exercises_miracast_discovery() {
        let (_server, client) = simulated_network_manager().await;
        let devices = discover_miracast_on_connection(&client, Duration::ZERO)
            .await
            .expect("simulated Miracast discovery");
        assert_eq!(devices.len(), 1);
        let receiver = &devices[0];
        assert_eq!(receiver.protocol, ProjectionProtocol::Miracast);
        assert_eq!(receiver.name, "Conference display");
        assert_eq!(receiver.id, "Miracast:02:00:00:00:00:09");
        assert_eq!(receiver.addresses, ["02:00:00:00:00:09"]);
        assert!(receiver.detail.contains("87%"));
    }

    fn thread_ids() -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/task")
            .expect("/proc/self/task")
            .map(|entry| entry.expect("task").file_name().into_string().expect("tid"))
            .collect()
    }

    fn context_switches(tid: &str) -> u64 {
        std::fs::read_to_string(format!("/proc/self/task/{tid}/status"))
            .expect("thread status")
            .lines()
            .filter(|line| line.contains("ctxt_switches"))
            .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
            .sum()
    }

    fn spawned_thread<T>(comm: &str, start: impl FnOnce() -> T) -> (T, String) {
        let before = thread_ids();
        let started = start();
        let waiting = Instant::now();
        loop {
            let named = thread_ids()
                .difference(&before)
                .find(|tid| {
                    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                        .is_ok_and(|name| name.trim() == comm)
                })
                .cloned();
            if let Some(tid) = named {
                return (started, tid);
            }
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "no new thread named {comm}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_stays_asleep(tid: &str, what: &str) {
        std::thread::sleep(Duration::from_millis(50));
        let settled = context_switches(tid);
        std::thread::sleep(Duration::from_millis(400));
        let woke = context_switches(tid).saturating_sub(settled);
        assert_eq!(woke, 0, "idle {what} woke {woke} times in 400 ms");
    }

    #[test]
    fn idle_http_accept_thread_sleeps_without_clients() {
        let stop = Arc::new(AtomicBool::new(false));
        let receiver = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let ((port, thread), tid) = spawned_thread("tilo-proj-http", || {
            start_http_server(
                std::env::temp_dir(),
                stop.clone(),
                receiver,
                "token".to_owned(),
                Arc::new(MediaTelemetry::default()),
            )
            .expect("HTTP server")
        });
        assert_stays_asleep(&tid, "projection HTTP accept");
        stop.store(true, Ordering::Release);
        wake_http_listener(receiver, port);
        thread.join().expect("join HTTP server");
    }
}
