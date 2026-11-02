//! The device VPN: a `NetworkManager` VPN profile the person set a kill
//! switch on, which is set to connect, drops — nothing leaves this device
//! while it is down, and what Settings shows next to the profile's switch
//! says so — and the route through another of their devices, which carries
//! the networks that device is on and comes down when they choose it off.
//!
//! The profile, the helper and the other device are a test's:
//! `NetworkManager` hands the reading in, the root helper is a socket that
//! records what it was asked and answers as the real one does, and the exit
//! is an endpoint on this machine's loopback whose tunnel is in memory.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use device_vpn::fdpass;
use device_vpn::net::Ipv4Net;
use device_vpn::packet;
use device_vpn::profile_block::{Sources, Vpns};
use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, Request, Response, Routes as LinkRoutes};
use device_vpn::tunnel::device::testing::{ChannelDevice, Stack, channel_device};
use device_vpn::tunnel::dns::DnsRelay;
use device_vpn::tunnel::{ALPN, Authority, Decision, Exit, ExitDevices};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use settings_store::device_vpn::{
    ClientState, DeviceVpnStatus, PROFILE_KILL_SWITCHES, ProfileBlockStatus, ProfileKillSwitch,
    Routes, THROUGH, Through, status_path,
};
use tilo_compositor::OutputConfig;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_device_fabric::{ClipboardEntry, DeviceFabric, DeviceId, PersonalFabricAction};
use tilo_nm::{ActiveState, ActiveVpn, VpnReading, VpnWatch};
use tilo_session::fabric::MediaContinuity;
use tilo_session::sync::{Client, OwnPresence};
use tilo_session::{Options, Session, Vpn};

const PROFILE: &str = "9b1c6f42-7c0a-4b0f-9a1c-2d3e4f5a6b7c";
/// The network the other device is on.
const HOME: &str = "192.168.1.0/24";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 700;
/// How long a step of the test waits for what it waits for.
const DEADLINE: Duration = Duration::from_secs(40);

/// The helper as a test knows it: the requests it was asked, in order, and
/// this device's end of each tunnel it opened.
#[derive(Clone, Default)]
struct Helper {
    asked: Arc<Mutex<Vec<Request>>>,
    tuns: Arc<Mutex<Vec<UnixDatagram>>>,
}

impl Helper {
    /// A helper listening at `socket`, answering every request as carried
    /// out.
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
                    // A tunnel: the helper holds one end and hands the
                    // other over with the answer, as it hands over a TUN.
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

    /// Whether a block was asked for that keeps the traffic that is not the
    /// profile's own in.
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

    /// This device's stack, as the helper left it open: what is written here
    /// leaves through the tunnel.
    fn tun(&self) -> UnixDatagram {
        let tuns = self.tuns.lock().unwrap();
        tuns.first()
            .and_then(|tun| tun.try_clone().ok())
            .expect("the helper opened a tunnel")
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

/// Wait for `condition`, or fail saying what was waited for.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("a deadline");
    while !condition() {
        assert!(Instant::now() < deadline, "waited for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What Settings reads: the status a running session published, read as the
/// VPN pane reads it, so a status Settings would take for a dead session's
/// is one this does not see either.
fn published_status() -> Option<DeviceVpnStatus> {
    DeviceVpnStatus::load()
}

/// What Settings shows next to the profiles' switches.
fn published() -> Option<ProfileBlockStatus> {
    published_status().map(|status| status.profile_block)
}

/// The person turned the profile's kill switch on and it drops: the helper
/// is told to block everything but the profile's servers, and what Settings
/// shows says the profile blocks until it is back.
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
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            vpn: Some(Vpn::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
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

    // It drops: `NetworkManager` says it is gone, and the person did not
    // turn it off.
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

    // The profile comes back: the block lifts and Settings stops saying it
    // blocks.
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

/// The person's other device, as far as this session sees it: an endpoint on
/// this machine's loopback offering its home network, whose tunnel is an
/// in-memory device the test holds.
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
                rt.block_on(async { tokio::time::timeout(DEADLINE, self.stacks.recv()).await })
                    .expect("the other device opens its tunnel")
                    .expect("the other device opened its tunnel"),
            );
        }
        let stack = self.stack.as_mut().expect("the other device's stack");
        rt.block_on(async { tokio::time::timeout(DEADLINE, stack.delivered()).await })
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

/// The person's devices as this session knows them: none, since the route
/// follows the person's choice whether or not their other device is heard
/// here.
#[derive(Clone, Default)]
struct NoDevices;

impl FabricLink for NoDevices {
    fn device_fabric(&self) -> DeviceFabric {
        DeviceFabric::default()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        BTreeSet::new()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        None
    }

    fn send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        Err("no transport in a test".to_owned())
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
    let deadline = Instant::now().checked_add(DEADLINE).expect("a deadline");
    while !done() {
        assert!(Instant::now() < deadline, "waited for {what}");
        session.step(Duration::from_millis(10)).expect("a frame");
    }
}

/// The person chooses to route their home network through their other
/// device: the tunnel comes up for the networks that device is on, carries
/// their traffic there, and Settings says which device carries what. They
/// turn it off, and the tunnel goes.
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

    // What the person chose in Settings: this device's home network, through
    // that device.
    THROUGH
        .set(Some(Through {
            routes: Routes::HomeNetwork,
            ..Through::device(hex::encode(exit_id.as_bytes()))
        }))
        .expect("the route is on");

    // This session's own endpoint, told where the other device is.
    let sync = Client::start(
        None,
        None,
        OwnPresence::default(),
        Vec::new(),
        MediaContinuity::default(),
        Arc::new(|| {}),
    )
    .expect("a loopback endpoint binds");
    sync.connect(exit.addr.clone());
    let fabric: SharedLink = Arc::new(NoDevices);

    let (sender, receiver) = tokio::sync::watch::channel(reading(None));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            sync: Some(Rc::new(sync)),
            fabric: Some(fabric),
            vpn: Some(Vpn::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
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

    // The person turns the route off: the tunnel goes and Settings stops
    // saying it routes.
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
