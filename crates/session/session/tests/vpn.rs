//! The device VPN: a `NetworkManager` VPN profile the person set a kill switch on, which is set to
//! connect, drops — nothing leaves this device while it is down, and what Settings shows next to
//! the profile's switch says so — and the route through another of their devices, which carries the
//! networks that device is on and comes down when they choose it off.
//! The profile, the helper and the other device are a test's: `NetworkManager` hands the reading
//! in, the root helper is a socket that records what it was asked and answers as the real one does,
//! and the exit is an endpoint on this machine's loopback whose tunnel is in memory.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_vpn::fdpass;
use device_vpn::net::Ipv4Net;
use device_vpn::packet;
use device_vpn::profile_block::{Sources, Vpns};
use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, Request, Response, Routes as LinkRoutes};
use device_vpn::tunnel::device::testing::{ChannelDevice, Stack, channel_device};
use device_vpn::tunnel::dns::DnsRelay;
use device_vpn::tunnel::{
    ALPN, Authority, ClientError, Decision, Ended, Exit, ExitDevices, Refusal, client,
};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use settings_store::device_vpn::{
    ClientState, DeviceVpnStatus, EXIT, PROFILE_KILL_SWITCHES, ProfileBlockStatus,
    ProfileKillSwitch, Refusal as ShownRefusal, Routes, STOP, StopRequest, THROUGH, Through,
    status_path,
};
use tilo_compositor::OutputConfig;
use tilo_nm::{ActiveState, ActiveVpn, VpnReading, VpnWatch};
use tilo_session::platform::fabric::Continuity;
use tilo_session::platform::fabric::SpeakerFabricState;
use tilo_session::platform::fabric::UnlockFabricState;
use tilo_session::platform::fabric::{
    ClipboardEntry, DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
};
use tilo_session::platform::fabric::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_session::sync::{Client, Entry, OwnPresence};
use tilo_session::{Options, Session, Vpn};
use tilo_settings_service::SettingsService;

const PROFILE: &str = "9b1c6f42-7c0a-4b0f-9a1c-2d3e4f5a6b7c";
/// The network the other device is on.
const HOME: &str = "192.168.1.0/24";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 700;

/// The helper as a test knows it: the requests it was asked, in order, and this device's end of
/// each tunnel it opened.
#[derive(Clone, Default)]
struct Helper {
    asked: Arc<Mutex<Vec<Request>>>,
    tuns: Arc<Mutex<Vec<UnixDatagram>>>,
}

impl Helper {
    /// A helper listening at `socket` , answering every request as carried out.
    fn at(socket: &Path) -> Self {
        let listener = UnixListener::bind(socket).expect("the helper listens");
        let helper = Self::default();
        let asked = Arc::clone(&helper.asked);
        let tuns = Arc::clone(&helper.tuns);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let envelope: Envelope = serde_json::from_str(&line).expect("a request");
                assert_eq!(envelope.protocol, PROTOCOL_VERSION);
                match envelope.request {
                    // A tunnel: the helper holds one end and hands the other over with the answer,
                    // as it hands over a TUN.
                    Request::OpenClient { link } => {
                        let (ours, theirs) = UnixDatagram::pair().expect("a tunnel");
                        tuns.lock().unwrap().push(ours);
                        asked
                            .lock()
                            .unwrap()
                            .push(Request::OpenClient { link: link.clone() });
                        let answer = serde_json::to_string(&Response::Opened {
                            interface: "tilovpn-test".to_owned(),
                            mtu: link.mtu,
                        })
                        .expect("an answer");
                        fdpass::write_line_with_fd(
                            &stream,
                            format!("{answer}\n").as_bytes(),
                            Some(theirs.as_fd()),
                        )
                        .expect("the opened answer");
                    }
                    // This device's exit: the same, for the other end of a tunnel one of the
                    // person's devices opens here.
                    Request::OpenExit { link } => {
                        let (ours, theirs) = UnixDatagram::pair().expect("an exit tunnel");
                        tuns.lock().unwrap().push(ours);
                        asked
                            .lock()
                            .unwrap()
                            .push(Request::OpenExit { link: link.clone() });
                        let answer = serde_json::to_string(&Response::Opened {
                            interface: "tiloexit-test".to_owned(),
                            mtu: link.mtu,
                        })
                        .expect("an answer");
                        fdpass::write_line_with_fd(
                            &stream,
                            format!("{answer}\n").as_bytes(),
                            Some(theirs.as_fd()),
                        )
                        .expect("the opened answer");
                    }
                    request => {
                        asked.lock().unwrap().push(request);
                        stream
                            .write_all(b"{\"result\":\"done\"}\n")
                            .expect("the answer");
                    }
                }
            }
        });
        helper
    }

    /// Whether a block was asked for that keeps the traffic that is not the profile's own in.
    fn holds_a_block(&self) -> Option<bool> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .find_map(|request| match request {
                Request::HoldProfileBlock { block } => Some(block.allow_local),
                _ => None,
            })
    }

    /// The tunnel the helper was asked to open, as this device asked for it.
    fn opened(&self) -> Option<Request> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .find(|request| matches!(request, Request::OpenClient { .. }))
            .cloned()
    }

    /// Whether the helper was asked to undo the tunnel.
    fn closed(&self) -> bool {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::CloseClient))
    }

    /// The request the helper was asked to open this device's exit with.
    fn exit_opened(&self) -> Option<Request> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .find(|request| matches!(request, Request::OpenExit { .. }))
            .cloned()
    }

    /// Whether the helper was asked to close this device's exit.
    fn exit_closed(&self) -> bool {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::CloseExit))
    }

    /// This device's stack, as the helper left it open: what is written here leaves through the
    /// tunnel.
    fn tun(&self) -> UnixDatagram {
        let tuns = self.tuns.lock().unwrap();
        tuns.first()
            .and_then(|tun| tun.try_clone().ok())
            .expect("the helper opened a tunnel")
    }

    /// The next packet the stack the helper opened received through it.
    fn received(&self) -> Vec<u8> {
        let tun = self.tun();
        tun.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("a timeout");
        let mut packet = vec![0_u8; 4096];
        let length = tun.recv(&mut packet).expect("the packet reaches the stack");
        packet.truncate(length);
        packet
    }
}

/// What `NetworkManager` says: the profile up, then gone.
fn reading(state: Option<ActiveState>) -> VpnReading {
    VpnReading {
        known: true,
        active: state.map_or_else(Vec::new, |state| {
            vec![ActiveVpn {
                uuid: PROFILE.to_owned(),
                state,
            }]
        }),
        ended_on_purpose: BTreeMap::new(),
    }
}

/// Wait for `condition` , or fail saying what was waited for.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    tilo_test_env::wait_until(what, condition);
}

/// What Settings reads: the status a running session published, read as the VPN pane reads it, so a
/// status Settings would take for a dead session's is one this does not see either.
fn published_status() -> Option<DeviceVpnStatus> {
    DeviceVpnStatus::load()
}

/// What Settings shows next to the profiles' switches.
fn published() -> Option<ProfileBlockStatus> {
    published_status().map(|status| status.profile_block)
}

/// The person turned the profile's kill switch on and it drops: the helper is told to block
/// everything but the profile's servers, and what Settings shows says the profile blocks until it
/// is back.
#[test]
fn a_profile_that_drops_blocks_traffic_and_says_so() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    PROFILE_KILL_SWITCHES
        .set(BTreeMap::from([(
            PROFILE.to_owned(),
            ProfileKillSwitch { allow_local: false },
        )]))
        .expect("the kill switch is on");
    let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
    let socket: PathBuf = runtime.path().join("helper.sock");
    let helper = Helper::at(&socket);

    let (sender, receiver) = tokio::sync::watch::channel(reading(Some(ActiveState::Activated)));
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            vpn: Some(Vpn::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
    );
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(status_path().is_some(), "this session can publish");
    wait_for("the session to publish its status", || {
        published().is_some()
    });
    assert_eq!(
        published(),
        Some(ProfileBlockStatus::default()),
        "at first the profile is up: nothing blocks, and Settings knows the session runs"
    );
    assert_eq!(helper.holds_a_block(), None, "so the helper is not asked");

    // It drops: `NetworkManager` says it is gone, and the person did not turn it off.
    sender.send(reading(None)).expect("the reading");

    wait_for("the helper to hold the block", || {
        helper.holds_a_block().is_some()
    });
    assert_eq!(
        helper.holds_a_block(),
        Some(false),
        "everything but the tunnel's own traffic is blocked"
    );
    wait_for("Settings to see what blocks", || {
        published().is_some_and(|block| block.blocking == [PROFILE])
    });
    let block = published().expect("a published block");
    assert!(
        !block.failed,
        "the helper holds it, so the device is not left open: {block:?}"
    );

    // The profile comes back: the block lifts and Settings stops saying it blocks.
    sender
        .send(reading(Some(ActiveState::Activated)))
        .expect("the reading");
    wait_for("the block to lift", || {
        helper
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::ReleaseProfileBlock))
    });
    wait_for("Settings to see the block lifted", || {
        published() == Some(ProfileBlockStatus::default())
    });
}

/// The person's other device, as far as this session sees it: an endpoint on this machine's
/// loopback offering its home network, whose tunnel is an in-memory device the test holds.
struct ExitFixture {
    exit: Exit<Everyones, Devices>,
    stacks: tokio::sync::mpsc::UnboundedReceiver<Stack>,
    /// The exit's stack, once it opened its tunnel.
    stack: Option<Stack>,
    router: iroh::protocol::Router,
    addr: EndpointAddr,
}

impl ExitFixture {
    /// An exit serving the tunnel ALPN on this machine's loopback.
    fn start(rt: &tokio::runtime::Runtime) -> Self {
        let (stacks, receivers) = tokio::sync::mpsc::unbounded_channel();
        let exit = Exit::new(
            Everyones,
            Devices { stacks },
            DnsRelay::new(SocketAddr::from((Ipv4Addr::LOCALHOST, 9))),
            || {},
        );
        let (router, addr) = rt.block_on(async {
            let server = Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback address")
                .bind()
                .await
                .expect("the other device's endpoint binds");
            let port = server
                .bound_sockets()
                .first()
                .expect("a bound socket")
                .port();
            let addr = EndpointAddr::new(server.id())
                .with_ip_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
            let router = iroh::protocol::Router::builder(server)
                .accept(ALPN, exit.clone())
                .spawn();
            (router, addr)
        });
        Self {
            exit,
            stacks: receivers,
            stack: None,
            router,
            addr,
        }
    }

    /// The other device, by the id this session dials it by.
    fn id(&self) -> EndpointId {
        self.addr.id
    }

    /// The next packet the other device's stack received through the tunnel.
    fn delivered(&mut self, rt: &tokio::runtime::Runtime) -> Vec<u8> {
        if self.stack.is_none() {
            self.stack = Some(
                rt.block_on(async {
                    tokio::time::timeout(tilo_test_env::hang_bound(), self.stacks.recv()).await
                })
                .expect("the other device opens its tunnel")
                .expect("the other device opened its tunnel"),
            );
        }
        let stack = self.stack.as_mut().expect("the other device's stack");
        rt.block_on(async {
            tokio::time::timeout(tilo_test_env::hang_bound(), stack.delivered()).await
        })
        .expect("the packet reaches the other device")
        .expect("the other device keeps its stack")
    }
}

/// The person's own devices, none of which this device's task knows of.
struct Everyones;

impl Authority for Everyones {
    fn decide(&self, _: &EndpointId) -> Decision {
        Decision::Allow
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        vec![HOME.parse().expect("a network")]
    }
}

/// The other device's tunnel, in memory.
struct Devices {
    stacks: tokio::sync::mpsc::UnboundedSender<Stack>,
}

impl ExitDevices for Devices {
    type Device = ChannelDevice;

    async fn open(&self, _mtu: u16) -> std::io::Result<ChannelDevice> {
        let (device, stack) = channel_device();
        let _ = self.stacks.send(stack);
        Ok(device)
    }

    async fn close(&self) {}
}

/// The person's devices as this session's fabric names them: what this session hears of them, since
/// a test runs no fabric of its own.
#[derive(Clone, Default)]
struct KnownDevices {
    /// Each device by the endpoint id it is dialled and reached by, and what the person calls it.
    named: Vec<(DeviceId, String)>,
}

impl KnownDevices {
    /// The person's devices `named` , as the fabric carries them.
    fn of(named: Vec<(DeviceId, String)>) -> Self {
        Self { named }
    }
}

impl FabricLink for KnownDevices {
    fn device_fabric(&self) -> DeviceFabric {
        let mut fabric = DeviceFabric::default();
        for (device, label) in &self.named {
            fabric
                .upsert(DeviceFabricSnapshot {
                    schema_version: FABRIC_SCHEMA_VERSION,
                    owner: device.clone(),
                    presence: DevicePresence {
                        device: device.clone(),
                        label: label.clone(),
                        kind: DeviceKind::Laptop,
                        capabilities: FabricCapabilities::default(),
                        online: true,
                        last_seen_unix_ms: 1,
                        wake_routes: Vec::new(),
                        session: OwnerSession::default(),
                        stamp: FabricStamp {
                            counter: 1,
                            device: device.clone(),
                        },
                    },
                    media: Vec::new(),
                    clipboard: Vec::new(),
                    settings: Vec::new(),
                    wifi: Vec::new(),
                    display: None,
                    speakers: SpeakerFabricState::default(),
                    unlock: UnlockFabricState::default(),
                })
                .expect("the snapshot is valid");
        }
        fabric
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        self.named
            .iter()
            .map(|(device, _)| device.clone())
            .collect()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        None
    }

    fn try_send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        Err(PersonalFabricSendError::Rejected("no transport".to_owned()))
    }

    fn require_media(&self) -> Result<(), String> {
        Ok(())
    }

    fn share_clipboard(&self, _: Vec<ClipboardEntry>) {}
}

/// Step the session until `done` holds, or fail saying what was waited for.
fn step_until(session: &mut Session, what: &str, done: impl Fn() -> bool) {
    tilo_test_env::wait_for(what, || {
        session.step(Duration::from_millis(10)).expect("a frame");
        done().then_some(())
    });
}

/// The person chooses to route their home network through their other device: the tunnel comes up
/// for the networks that device is on, carries their traffic there, and Settings says which device
/// carries what. They turn it off, and the tunnel goes.
#[test]
fn choosing_a_device_to_route_through_carries_its_networks_until_they_stop() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
    let socket: PathBuf = runtime.path().join("helper.sock");
    let helper = Helper::at(&socket);

    // The other device, up before this one asks to route through it.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let mut exit = ExitFixture::start(&rt);
    let exit_id = exit.id();

    // What the person chose in Settings: this device's home network, through that device.
    THROUGH
        .set(Some(Through {
            routes: Routes::HomeNetwork,
            ..Through::device(hex::encode(exit_id.as_bytes()))
        }))
        .expect("the route is on");

    // This session's own endpoint, told where the other device is.
    let sync = Client::start(
        Entry {
            identity: None,
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: Continuity::default(),
            wake: Arc::new(|| {}),
            fabric: None,
            relays: tilo_endpoint_reachability::relays(),
        },
        None,
        None,
        Vec::new(),
        None,
    )
    .expect("a loopback endpoint binds");
    sync.connect(exit.addr.clone());
    let fabric: SharedLink = Arc::new(KnownDevices::default());

    let (sender, receiver) = tokio::sync::watch::channel(reading(None));
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            sync: Some(Rc::new(sync)),
            fabric: Some(fabric),
            vpn: Some(Vpn::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
    );
    let _sender = sender;

    step_until(&mut session, "the tunnel to come up", || {
        helper.opened().is_some()
    });
    let Some(Request::OpenClient { link }) = helper.opened() else {
        panic!("the helper was asked to open this device's tunnel");
    };
    assert_eq!(
        link.routes,
        LinkRoutes::Networks {
            networks: vec![HOME.parse().expect("a network")],
        },
        "only the network the other device is on goes through it"
    );
    assert_eq!(
        link.address,
        Ipv4Addr::new(100, 100, 64, 2),
        "the address the other device gave this one"
    );
    assert!(link.dns, "names resolve on the other device's network");
    assert!(
        !link.kill_switch,
        "the person left the route's kill switch off"
    );
    assert!(
        !link.carrier.udp_ports.is_empty(),
        "the tunnel's own packets reach the exit outside the tunnel"
    );

    // Settings says which device carries what.
    step_until(&mut session, "Settings to show the route", || {
        published_status().is_some_and(|status| status.client.is_some())
    });
    let status = published_status().expect("a published status");
    let client = status.client.expect("the route");
    assert_eq!(client.device, hex::encode(exit_id.as_bytes()));
    assert!(
        matches!(client.state, ClientState::Connected { .. }),
        "the route is up: {client:?}"
    );
    assert_eq!(client.routes, Routes::HomeNetwork);
    assert!(!client.kill_switch);
    assert!(
        !status.exit_available,
        "this session serves no exit, and says so rather than offering a switch"
    );

    // The person's traffic for home leaves through the tunnel.
    let tun = helper.tun();
    let web = SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 7), 443);
    let local = SocketAddrV4::new(link.address, 50_000);
    let outward =
        packet::udp_packet(local, web, b"hello from home").expect("a packet to send through it");
    tun.send(&outward).expect("the packet goes into the tunnel");
    assert_eq!(
        exit.delivered(&rt),
        outward,
        "the packet arrives at the other device as this device sent it"
    );
    assert_eq!(
        exit.exit.sessions().len(),
        1,
        "the other device has this one's tunnel"
    );

    // The person turns the route off: the tunnel goes and Settings stops saying it routes.
    THROUGH.set(None).expect("the route is off");
    step_until(&mut session, "the helper to undo the tunnel", || {
        helper.closed()
    });
    step_until(&mut session, "Settings to show the route gone", || {
        published_status().is_some_and(|status| status.client.is_none())
    });

    let ExitFixture { router, .. } = exit;
    rt.block_on(router.shutdown()).expect("the exit stops");
}

/// Who sits at both devices: the person's id, as the settings document records what they changed.
const PERSON: &str = "ada";
/// What the person calls the laptop they take with them.
const LAPTOP: &str = "Laptop";
/// And what they call this device.
const DESK: &str = "Desk";
/// And the phone they carry.
const PHONE: &str = "Phone";

/// Step both sessions until `done` holds, or fail saying what was waited for: two of the person's
/// devices, each with its own pass.
fn step_both_until(one: &mut Session, other: &mut Session, what: &str, done: impl Fn() -> bool) {
    tilo_test_env::wait_for(what, || {
        one.step(Duration::from_millis(10)).expect("a frame");
        other.step(Duration::from_millis(10)).expect("a frame");
        done().then_some(())
    });
}

/// This device is an exit while the person turns it on, and only for their own devices: the laptop
/// they take with them routes through it and its traffic comes out here; a device that is not
/// theirs dialling the tunnel is turned away; Settings says who routes through this one, the person
/// stops them from here, and the laptop's choice comes off with it.
/// Both ends are this process's: the two sessions meet on this machine's loopback, as two of the
/// person's devices meet on their fabric.
#[test]
fn this_device_carries_the_persons_other_devices_and_turns_a_stranger_away() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
    let desk_socket: PathBuf = runtime.path().join("desk-helper.sock");
    let desk_helper = Helper::at(&desk_socket);
    let laptop_socket: PathBuf = runtime.path().join("laptop-helper.sock");
    let laptop_helper = Helper::at(&laptop_socket);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");

    // This device's endpoint, with the exit on it: what answers another of the person's devices
    // asking to route through this one.
    let (_desk_reading, desk_vpns) = tokio::sync::watch::channel(reading(None));
    let desk_vpn = Vpn::with(Sources {
        helper: desk_socket,
        vpns: Vpns::Fed(VpnWatch::following(desk_vpns)),
    });
    let desk_sync = Client::start(
        Entry {
            identity: None,
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: Continuity::default(),
            wake: Arc::new(|| {}),
            fabric: None,
            relays: tilo_endpoint_reachability::relays(),
        },
        None,
        None,
        desk_vpn.served(),
        None,
    )
    .expect("this device's endpoint binds");
    // The laptop's endpoint, serving nothing but its own planes: the route it carries is not one of
    // them.
    let (_laptop_reading, laptop_vpns) = tokio::sync::watch::channel(reading(None));
    let laptop_sync = Client::start(
        Entry {
            identity: None,
            own: OwnPresence::default(),
            hosts: Vec::new(),
            continuity: Continuity::default(),
            wake: Arc::new(|| {}),
            fabric: None,
            relays: tilo_endpoint_reachability::relays(),
        },
        None,
        None,
        Vec::new(),
        None,
    )
    .expect("the laptop's endpoint binds");
    // The person's phone: an endpoint that proves which of their devices it is by the key it
    // reaches this one with, as their devices do.
    let phone = rt.block_on(async {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("the phone's endpoint binds")
    });
    let phone_device = DeviceId::new(hex::encode(phone.id().as_bytes())).expect("the phone's id");
    let desk_addr = desk_sync.endpoint_addr();
    let desk_device =
        DeviceId::new(hex::encode(desk_sync.publisher().as_bytes())).expect("this device's id");
    let laptop_id = laptop_sync.publisher();
    let laptop_device = DeviceId::new(hex::encode(laptop_id.as_bytes())).expect("the laptop's id");
    // The laptop was told where this device is: its route dials the tunnel by the same id.
    laptop_sync.connect(desk_addr.clone());

    let mut laptop = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            user: PERSON.to_owned(),
            services: vec![Box::new(SettingsService::new())],
            sync: Some(Rc::new(laptop_sync)),
            fabric: Some(Arc::new(KnownDevices::of(vec![(
                desk_device.clone(),
                DESK.to_owned(),
            )]))),
            vpn: Some(Vpn::with(Sources {
                helper: laptop_socket,
                vpns: Vpns::Fed(VpnWatch::following(laptop_vpns)),
            })),
            ..Options::default()
        },
    );
    let mut desk = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            user: PERSON.to_owned(),
            services: vec![Box::new(SettingsService::new())],
            sync: Some(Rc::new(desk_sync)),
            fabric: Some(Arc::new(KnownDevices::of(vec![
                (laptop_device.clone(), LAPTOP.to_owned()),
                (phone_device.clone(), PHONE.to_owned()),
            ]))),
            vpn: Some(desk_vpn),
            ..Options::default()
        },
    );

    // This device can carry an exit: its helper is installed and its endpoint answers the tunnel.
    // Settings here says so, and that the person has not turned it on.
    step_until(&mut desk, "this device to say it can carry an exit", || {
        published_status().is_some_and(|status| status.exit_available && !status.exit_on)
    });

    // The person chooses the laptop's route before they turn their exit on: a route asked for here
    // is refused for that reason.
    THROUGH
        .set(Some(Through::device(desk_device.as_str())))
        .expect("the laptop routes through this device");
    step_until(&mut laptop, "the route to be refused", || {
        published_status()
            .and_then(|status| status.client)
            .is_some_and(|client| {
                client.device == desk_device.as_str()
                    && client.state
                        == ClientState::Refused {
                            reason: ShownRefusal::ExitOff,
                        }
            })
    });
    let status = published_status().expect("a published status");
    assert_eq!(
        status.client.expect("the route").label,
        DESK,
        "the laptop's route says which of the person's devices it goes to"
    );

    // A device that is nobody's: it reaches this device's endpoint, since anyone may dial it, and
    // the tunnel turns it away.
    let stranger = rt.block_on(async {
        Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("a stranger's endpoint binds")
    });
    let refused = rt.block_on(async { client::connect(&stranger, desk_addr.clone()).await });
    assert_eq!(
        refused.err(),
        Some(ClientError::Refused(Refusal::NotYours)),
        "a device that is not the person's is told so, whatever this device's switch says"
    );
    rt.block_on(stranger.close());

    // The person turns their exit on, and chooses the route again: the laptop's tunnel comes up,
    // through this device.
    EXIT.set(true).expect("the exit is on");
    step_until(&mut desk, "this device to say its exit is on", || {
        published_status().is_some_and(|status| status.exit_available && status.exit_on)
    });

    // With the choice taken off, the laptop stops routing.
    THROUGH.set(None).expect("the choice comes off");
    step_until(&mut laptop, "Settings to show the route gone", || {
        published_status().is_some_and(|status| status.client.is_none())
    });
    THROUGH
        .set(Some(Through::device(desk_device.as_str())))
        .expect("the laptop routes through this device");

    step_until(&mut laptop, "the tunnel to come up", || {
        laptop_helper.opened().is_some() && desk_helper.exit_opened().is_some()
    });
    let Some(Request::OpenClient { link }) = laptop_helper.opened() else {
        panic!("the laptop asked its helper for the tunnel");
    };
    assert_eq!(
        link.routes,
        LinkRoutes::All,
        "everything the person sends goes through this device"
    );
    assert!(
        link.mtu <= device_vpn::tunnel::exit::EXIT_MTU && link.mtu >= device_vpn::net::MIN_MTU,
        "the tunnel's packets fit what this device's exit TUN carries: {}",
        link.mtu
    );
    assert_eq!(
        desk_helper.exit_opened(),
        Some(Request::OpenExit {
            link: device_vpn::protocol::ExitLink {
                mtu: device_vpn::tunnel::exit::EXIT_MTU,
            },
        }),
        "this device's exit TUN was opened for it"
    );

    // Settings on the laptop says the route is up, through this device.
    step_until(&mut laptop, "the route to be up", || {
        published_status()
            .and_then(|status| status.client)
            .is_some_and(|client| matches!(client.state, ClientState::Connected { .. }))
    });
    let status = published_status().expect("a published status");
    let client = status.client.expect("the route");
    assert_eq!(client.device, desk_device.as_str());
    assert_eq!(
        client.label, DESK,
        "by the name the person gave this device"
    );
    assert_eq!(client.routes, Routes::All);

    // What the laptop sends leaves through this device.
    let tun = laptop_helper.tun();
    let web = SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 7), 443);
    let outward = packet::udp_packet(
        SocketAddrV4::new(link.address, 50_000),
        web,
        b"hello through the desk",
    )
    .expect("a packet to send through the tunnel");
    tun.send(&outward).expect("the packet goes into the tunnel");
    assert_eq!(
        desk_helper.received(),
        outward,
        "the packet leaves through this device as the laptop sent it"
    );

    // The person's phone routes through this device as well: Settings here says both of their
    // devices do, by the names they gave them.
    let phone_tunnel = rt
        .block_on(async { client::connect(&phone, desk_addr.clone()).await })
        .expect("the phone routes through this device");
    step_until(&mut desk, "Settings to show both devices", || {
        published_status().is_some_and(|status| status.exit_users.len() == 2)
    });
    let status = published_status().expect("a published status");
    assert!(
        status.exit_available,
        "this device says it can carry an exit"
    );
    assert!(status.exit_on, "and that the person turned it on");
    let users: Vec<(&str, &str)> = status
        .exit_users
        .iter()
        .map(|user| (user.device.as_str(), user.label.as_str()))
        .collect();
    assert_eq!(
        users,
        [
            (laptop_device.as_str(), LAPTOP),
            (phone_device.as_str(), PHONE),
        ],
        "both of the person's devices route through this one, by their names"
    );
    assert!(
        status.exit_users.iter().all(|user| user.since_unix_ms != 0),
        "and since when"
    );

    // The person stops the phone from here: its tunnel ends telling it why,
    // and Settings says only the laptop routes through this one now.
    let before = settings_changes().len();
    STOP.set(Some(StopRequest::after(None, phone_device.as_str())))
        .expect("the person stops the phone routing through this device");
    step_until(&mut desk, "the phone's tunnel to go", || {
        published_status().is_some_and(|status| {
            status.exit_users.len() == 1 && status.exit_users[0].device == laptop_device.as_str()
        })
    });
    let ended = rt
        .block_on(async {
            tokio::time::timeout(
                tilo_test_env::hang_bound(),
                phone_tunnel.connection.closed(),
            )
            .await
        })
        .expect("the phone's tunnel ends");
    assert_eq!(
        Ended::of(&ended),
        Ended::Refused(Refusal::Stopped),
        "this device told the phone that the person stopped it"
    );

    // They stop the laptop too: its tunnel goes, and its choice comes off through the same action
    // they made it with, not behind Settings' back.
    let stopped = StopRequest::after(STOP.get().as_ref(), laptop_device.as_str());
    STOP.set(Some(stopped))
        .expect("the person stops the laptop routing through this device too");
    step_both_until(&mut laptop, &mut desk, "the tunnel to go", || {
        laptop_helper.closed()
    });
    step_both_until(&mut laptop, &mut desk, "Settings to show no route", || {
        published_status()
            .is_some_and(|status| status.client.is_none() && status.exit_users.is_empty())
    });
    step_both_until(
        &mut laptop,
        &mut desk,
        "this device's exit to close",
        || desk_helper.exit_closed(),
    );
    let cleared = settings_changes()
        .into_iter()
        .skip(before)
        .find(|change| change["action"] == serde_json::json!("settings.device-vpn/route-through"))
        .expect("the laptop's choice came off through the action that made it");
    assert_eq!(
        cleared["principal"],
        serde_json::json!(PERSON),
        "the person's own change, not a file written behind their back"
    );
    assert_eq!(
        THROUGH.get(),
        None,
        "the choice is off, as the person finds it in Settings"
    );

    rt.block_on(async {
        phone.close().await;
    });
}

/// The settings document's history, oldest first.
fn settings_changes() -> Vec<serde_json::Value> {
    settings_store::local()
        .expect("the person's settings")
        .change_log()
}
