//! A whole Miracast session without hardware or a desktop: the `ScreenCast`
//! portal is simulated on a private session bus and grants a live node of a
//! headless `PipeWire` screen, `NetworkManager` is simulated on a private
//! connection, audio comes from the test environment's headless `PulseAudio`,
//! and the receiver is the `miracast` crate's strict test sink. Everything
//! from the portal call to the RTP stream is the production path.

#![allow(
    clippy::unused_self,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    reason = "the simulated D-Bus objects' methods take the shape of their interfaces' signatures"
)]

use std::collections::HashMap;
use std::io::BufRead as _;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use miracast::headless::HeadlessScreen;
use miracast::media::AudioInput;
use miracast::sink::{SinkCapabilities, TestSink};
use miracast::ts::{PID_AUDIO, PID_VIDEO, STREAM_TYPE_AAC_ADTS, STREAM_TYPE_H264};
use tilo_projector::control::{ConnectStage, ProjectorEvent};
use tilo_projector::{
    DisplayStreamPolicy, ProjectionDevice, ProjectionMode, ProjectionOptions, ProjectionProtocol,
};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::{Environment, PLAYING, run};
use crate::session::Helpers;
use crate::session::context::SessionContext;
use crate::session::paths::SessionPaths;

const SINK_MAC: &str = "02:00:00:00:00:09";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// A private session bus.
struct SessionBus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl SessionBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().expect("bus directory");
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                directory.path().join("bus").display()
            ))
            .stdout(Stdio::piped())
            .spawn()
            .expect("the session bus needs dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().expect("dbus-daemon output"))
            .read_line(&mut address)
            .expect("dbus-daemon address");
        Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[derive(Default)]
struct PortalRecord {
    selected_types: Option<u32>,
    sessions_closed: u32,
}

/// The `org.freedesktop.portal.ScreenCast` frontend, granting the headless
/// screen the way xdg-desktop-portal grants a monitor once the person chose.
struct Portal {
    screen: Arc<HeadlessScreen>,
    record: Arc<Mutex<PortalRecord>>,
}

fn sender_component(header: &zbus::message::Header<'_>) -> String {
    header
        .sender()
        .map(|sender| sender.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default()
}

fn token(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|value| String::try_from(value.try_clone().ok()?).ok())
        .unwrap_or_else(|| "t".to_owned())
}

async fn respond(
    connection: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    options: &HashMap<String, OwnedValue>,
    results: HashMap<&str, Value<'_>>,
) -> OwnedObjectPath {
    let path = format!(
        "{PORTAL_PATH}/request/{}/{}",
        sender_component(header),
        token(options, "handle_token")
    );
    let request = OwnedObjectPath::try_from(path).expect("request path");
    SignalEmitter::new(connection, request.clone())
        .expect("emitter")
        .emit(
            "org.freedesktop.portal.Request",
            "Response",
            &(0_u32, results),
        )
        .await
        .expect("Response signal");
    request
}

#[zbus::interface(name = "org.freedesktop.portal.ScreenCast")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        5
    }

    #[zbus(property)]
    fn available_source_types(&self) -> u32 {
        7
    }

    #[zbus(property)]
    fn available_cursor_modes(&self) -> u32 {
        7
    }

    async fn create_session(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let session = format!(
            "{PORTAL_PATH}/session/{}/{}",
            sender_component(&header),
            token(&options, "session_handle_token")
        );
        server
            .at(
                session.as_str(),
                PortalSession {
                    record: Arc::clone(&self.record),
                },
            )
            .await
            .expect("session object");
        respond(
            connection,
            &header,
            &options,
            HashMap::from([("session_handle", Value::from(session.as_str()))]),
        )
        .await
    }

    async fn select_sources(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        self.record.lock().expect("record").selected_types = options
            .get("types")
            .and_then(|value| u32::try_from(value.try_clone().ok()?).ok());
        respond(connection, &header, &options, HashMap::new()).await
    }

    async fn start(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _session: ObjectPath<'_>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> OwnedObjectPath {
        let stream: (u32, HashMap<&str, Value<'_>>) = (
            self.screen.node(),
            HashMap::from([
                ("source_type", Value::from(1_u32)),
                ("size", Value::from((1280_i32, 720_i32))),
            ]),
        );
        respond(
            connection,
            &header,
            &options,
            HashMap::from([("streams", Value::from(vec![stream]))]),
        )
        .await
    }

    fn open_pipe_wire_remote(
        &self,
        _session: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        self.screen
            .connect()
            .map(Into::into)
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))
    }
}

struct PortalSession {
    record: Arc<Mutex<PortalRecord>>,
}

#[zbus::interface(name = "org.freedesktop.portal.Session")]
impl PortalSession {
    fn close(&self) {
        let mut record = self.record.lock().expect("record");
        record.sessions_closed = record.sessions_closed.saturating_add(1);
    }
}

#[derive(Default)]
struct NmRecord {
    wfd_ies: Vec<u8>,
    deactivated: u32,
}

const P2P_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/2";
const GROUP_DEVICE: &str = "/org/freedesktop/NetworkManager/Devices/3";
const PEER: &str = "/org/freedesktop/NetworkManager/WifiP2PPeer/9";
const ACTIVE: &str = "/org/freedesktop/NetworkManager/ActiveConnection/4";
const IP4: &str = "/org/freedesktop/NetworkManager/IP4Config/5";

fn path(value: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(value).expect("object path")
}

struct Manager(Arc<Mutex<NmRecord>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![path(P2P_DEVICE)]
    }

    fn add_and_activate_connection2(
        &self,
        settings: HashMap<String, HashMap<String, OwnedValue>>,
        _device: ObjectPath<'_>,
        _peer: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> (
        OwnedObjectPath,
        OwnedObjectPath,
        HashMap<String, OwnedValue>,
    ) {
        self.0.lock().expect("record").wfd_ies = settings
            .get("wifi-p2p")
            .and_then(|p2p| p2p.get("wfd-ies"))
            .and_then(|ies| Vec::<u8>::try_from(ies.try_clone().ok()?).ok())
            .unwrap_or_default();
        (
            path("/org/freedesktop/NetworkManager/Settings/7"),
            path(ACTIVE),
            HashMap::new(),
        )
    }

    fn deactivate_connection(&self, _active: ObjectPath<'_>) {
        let mut record = self.0.lock().expect("record");
        record.deactivated = record.deactivated.saturating_add(1);
    }
}

struct Device(u32, &'static str);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        self.0
    }

    #[zbus(property)]
    fn ip_interface(&self) -> String {
        self.1.to_owned()
    }

    #[zbus(property)]
    fn ip4_config(&self) -> OwnedObjectPath {
        path(IP4)
    }
}

struct P2pDevice;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.WifiP2P")]
impl P2pDevice {
    fn start_find(&self, _options: HashMap<String, OwnedValue>) {}

    fn stop_find(&self) {}

    #[zbus(property)]
    fn peers(&self) -> Vec<OwnedObjectPath> {
        vec![path(PEER)]
    }
}

struct Peer;

#[zbus::interface(name = "org.freedesktop.NetworkManager.WifiP2PPeer")]
impl Peer {
    #[zbus(property)]
    fn hw_address(&self) -> String {
        SINK_MAC.to_owned()
    }

    #[zbus(property)]
    fn name(&self) -> String {
        "Living Room TV".to_owned()
    }

    #[zbus(property, name = "WfdIEs")]
    fn wfd_ies(&self) -> Vec<u8> {
        vec![0x00, 0x00, 0x06, 0x00, 0x11, 0x1c, 0x44, 0x00, 0x32]
    }
}

struct Active;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn devices(&self) -> Vec<OwnedObjectPath> {
        vec![path(GROUP_DEVICE)]
    }
}

/// The group's address: loopback, where the test sink runs.
struct Ip4;

#[zbus::interface(name = "org.freedesktop.NetworkManager.IP4Config")]
impl Ip4 {
    #[zbus(property)]
    fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
        vec![HashMap::from([
            (
                "address".to_owned(),
                OwnedValue::try_from(Value::from("127.0.0.1")).expect("value"),
            ),
            ("prefix".to_owned(), OwnedValue::from(8_u32)),
        ])]
    }

    #[zbus(property)]
    fn gateway(&self) -> String {
        String::new()
    }
}

async fn network_manager(record: Arc<Mutex<NmRecord>>) -> (zbus::Connection, zbus::Connection) {
    let (server, client) = tokio::net::UnixStream::pair().expect("socket pair");
    let server = Builder::unix_stream(server)
        .server(zbus::Guid::generate())
        .expect("guid")
        .p2p()
        .serve_at("/org/freedesktop/NetworkManager", Manager(record))
        .expect("manager")
        .serve_at(P2P_DEVICE, Device(30, "p2p-dev-wlp1s0"))
        .expect("device")
        .serve_at(P2P_DEVICE, P2pDevice)
        .expect("p2p device")
        .serve_at(PEER, Peer)
        .expect("peer")
        .serve_at(ACTIVE, Active)
        .expect("active connection")
        .serve_at(GROUP_DEVICE, Device(30, "p2p-wlp1s0-0"))
        .expect("group device")
        .serve_at(IP4, Ip4)
        .expect("ip4 config");
    let client = Builder::unix_stream(client).p2p();
    tokio::try_join!(server.build(), client.build()).expect("simulated NetworkManager")
}

fn free_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("free port")
}

fn connected(events: &Receiver<ProjectorEvent>) -> Vec<ProjectorEvent> {
    let mut seen = Vec::new();
    let deadline = tilo_test_env::hang_deadline();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(event) => {
                let done = matches!(event, ProjectorEvent::Connected { .. });
                seen.push(event);
                if done {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    seen
}

/// The simulated `NetworkManager` (server and client ends) and portal, and a
/// session-bus connection for the session to call the portal on.
fn simulations(
    bus: &SessionBus,
    screen: &Arc<HeadlessScreen>,
    portal_record: &Arc<Mutex<PortalRecord>>,
    nm_record: &Arc<Mutex<NmRecord>>,
) -> (
    zbus::Connection,
    zbus::Connection,
    zbus::Connection,
    zbus::Connection,
) {
    tilo_rt::app()
        .block_on(async {
            let (nm_server, system) = network_manager(Arc::clone(nm_record)).await;
            let portal = Builder::address(bus.address.as_str())
                .expect("bus address")
                .name("org.freedesktop.portal.Desktop")
                .expect("portal name")
                .serve_at(
                    PORTAL_PATH,
                    Portal {
                        screen: Arc::clone(screen),
                        record: Arc::clone(portal_record),
                    },
                )
                .expect("portal")
                .build()
                .await
                .expect("portal connection");
            let session = Builder::address(bus.address.as_str())
                .expect("bus address")
                .build()
                .await
                .expect("session connection");
            (nm_server, system, portal, session)
        })
        .expect("runtime")
}

fn miracast_receiver() -> (ProjectionDevice, ProjectionOptions) {
    let device = ProjectionDevice {
        id: format!("Miracast:{SINK_MAC}"),
        name: "Living Room TV".to_owned(),
        protocol: ProjectionProtocol::Miracast,
        detail: "Wi-Fi Direct".to_owned(),
        modes: vec![ProjectionMode::Mirror],
        addresses: vec![SINK_MAC.to_owned()],
        port: 0,
    };
    let options = ProjectionOptions {
        audio: true,
        stream_policy: DisplayStreamPolicy {
            max_fps: 30,
            ..DisplayStreamPolicy::default()
        },
        ..ProjectionOptions::default()
    };
    (device, options)
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[test]
fn a_miracast_session_casts_the_portal_screen_with_audio_to_a_receiver() {
    tilo_test_env::require_capability!(
        ScreenCapture,
        ["pipewire", "wireplumber", "pw-dump"]
            .iter()
            .all(|program| on_path(program)),
        "the headless screen needs PipeWire, WirePlumber and pw-dump"
    );
    let screen = Arc::new(HeadlessScreen::start(1280, 720, 30).expect("headless screen"));
    let bus = SessionBus::start();
    let portal_record = Arc::new(Mutex::new(PortalRecord::default()));
    let nm_record = Arc::new(Mutex::new(NmRecord::default()));
    let (_nm_server, system, _portal, session) =
        simulations(&bus, &screen, &portal_record, &nm_record);
    let rtsp = free_port();
    let environment = Environment {
        system,
        session,
        rtsp,
        audio: AudioInput::DefaultMonitor,
    };

    let root: PathBuf =
        std::env::temp_dir().join(format!("tilo-projector-miracast-{}", std::process::id()));
    let paths = SessionPaths::for_root(root.clone()).expect("session paths");
    let (context, events) = SessionContext::for_test(paths, Helpers::from_env());
    let (device, options) = miracast_receiver();

    std::thread::scope(|scope| {
        let session = scope.spawn(|| run(&device, &options, &context, &environment));
        // The receiver connects once the session listens, as a TV does after
        // the group forms.
        let mut sink =
            tilo_test_env::wait_for(
                "the session's RTSP port",
                || match TestSink::connect_and_play(
                    rtsp,
                    &SinkCapabilities::default(),
                    tilo_test_env::hang_bound(),
                ) {
                    Ok(sink) => Some(sink),
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                        std::thread::sleep(Duration::from_millis(50));
                        None
                    }
                    Err(error) => panic!("the receiver could not negotiate: {error}"),
                },
            );
        let seen = connected(&events);
        assert!(
            seen.contains(&ProjectorEvent::Connecting {
                device_id: context.device_id.clone(),
                stage: ConnectStage::Handshake
            }),
            "{seen:?}"
        );
        assert!(
            matches!(seen.last(), Some(ProjectorEvent::Connected { .. })),
            "{PLAYING} was reported: {seen:?}"
        );
        assert_eq!(sink.negotiated().audio_codecs, "AAC 00000001 00");
        let streamed = sink
            .receive_until(tilo_test_env::hang_bound(), |sink| {
                sink.ts.idr_seen
                    && sink.ts.pes_count(PID_VIDEO) >= 15
                    && sink.ts.pes_count(PID_AUDIO) >= 5
            })
            .expect("receive");
        assert!(streamed, "the screen and its sound arrived: {:?}", sink.ts);
        assert_eq!(sink.ts.pid_of(STREAM_TYPE_H264), Some(PID_VIDEO));
        assert_eq!(sink.ts.pid_of(STREAM_TYPE_AAC_ADTS), Some(PID_AUDIO));

        // Disconnect: the source triggers the receiver's teardown, then
        // releases the group and the portal session.
        context.control().request_stop(None);
        sink.follow_teardown_trigger(tilo_test_env::hang_bound())
            .expect("teardown trigger");
        session
            .join()
            .expect("session thread")
            .expect("clean disconnect");
    });

    let nm = std::mem::take(&mut *nm_record.lock().expect("record"));
    assert_eq!(
        nm.wfd_ies,
        miracast::ie::source_ies(rtsp.port()),
        "the source announced its RTSP port"
    );
    assert_eq!(nm.deactivated, 1, "the group was taken down");
    let portal = std::mem::take(&mut *portal_record.lock().expect("record"));
    assert_eq!(
        portal.selected_types,
        Some(1 | 4),
        "monitors and virtual screens"
    );
    assert_eq!(portal.sessions_closed, 1, "the screen cast ended");
    let _ = std::fs::remove_dir_all(root);
}
