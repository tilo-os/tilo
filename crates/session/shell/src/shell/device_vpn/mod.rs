//! Your devices as a VPN, in the session (docs/device-vpn.md).
//!
//! * The exit: another of the person's devices reaches [`serve_connection`]
//!   over [`device_vpn::tunnel::ALPN`] on the shell's peer endpoint. It may
//!   route only while it is one of the person's devices and the person's
//!   switch ([`settings_store::device_vpn::EXIT`]) is on; the TUN comes from
//!   the root helper while anyone routes.
//! * The client: [`settings_store::device_vpn::THROUGH`] names the device to
//!   route through. A task on the network's runtime dials it, asks the helper
//!   for the link, carries the packets, and follows what
//!   [`controller`] decides each outcome means, kill switch included.
//! * Which devices offer themselves is learnt by asking each online device
//!   of the person, now and then, what it offers.
//! * The fabric tick ([`refresh`]) follows the settings and the person's
//!   devices, acts on a stop asked from Settings, and publishes a
//!   [`DeviceVpnStatus`] for Settings, from which quick settings and the
//!   palette read the same state ([`entries`], [`choose`], [`stop_device`]).

pub(crate) mod controller;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use device_vpn::net::{CLIENT_INTERFACE, EXIT_INTERFACE, Ipv4Net, is_relay_address};
use device_vpn::protocol::{
    self, Carrier, ClientLink, ExitLink, MAX_CARRIER_PORTS, MAX_RELAYS, Request, Response,
};
use device_vpn::tunnel::device::TunDevice;
use device_vpn::tunnel::dns::DnsRelay;
use device_vpn::tunnel::{Authority, Decision, Ended, Exit, ExitDevices, Tunnel, client};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use settings_store::device_vpn::{
    self as settings, ClientState, ClientStatus, DeviceVpnStatus, ExitUser, OfferedExit, Problem,
    StopRequest, Through,
};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tokio::sync::watch;

use crate::shell::network::ShellNetwork;
use controller::{AfterEnd, Want};

/// How long a request to the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the person's online devices are asked what they offer.
const PROBE_EVERY: Duration = Duration::from_secs(300);
/// What marks a quick settings VPN entry as one of these routes.
const ENTRY_PREFIX: &str = "tilo-device-vpn:";

/// Who may route through this device, as the exit decides.
#[derive(Default)]
struct View {
    /// The person's other devices, by endpoint key.
    devices: HashSet<[u8; 32]>,
    exit_on: bool,
}

#[derive(Clone, Default)]
struct ShellAuthority(Arc<RwLock<View>>);

impl Authority for ShellAuthority {
    fn decide(&self, peer: &EndpointId) -> Decision {
        let view = self.0.read_recover();
        if !view.devices.contains(peer.as_bytes()) {
            Decision::NotYours
        } else if !view.exit_on {
            Decision::ExitOff
        } else {
            Decision::Allow
        }
    }

    fn home_networks(&self) -> Vec<Ipv4Net> {
        home_networks()
    }
}

/// The networks this device is attached to, other than the tunnel's own.
fn home_networks() -> Vec<Ipv4Net> {
    crate::shell::device_wake::ipv4_subnets()
        .into_iter()
        .filter(|(name, _)| name != CLIENT_INTERFACE && name != EXIT_INTERFACE)
        .filter_map(|(_, subnet)| Ipv4Net::new(Ipv4Addr::from(subnet.network), subnet.prefix))
        .filter(|network| network.is_routable_home())
        .collect()
}

/// Ask the root helper; the answer may carry a TUN.
async fn ask_helper(request: Request) -> Result<(Response, Option<OwnedFd>), Problem> {
    tokio::task::spawn_blocking(move || {
        protocol::call(Path::new(protocol::SOCKET_PATH), &request, HELPER_TIMEOUT)
    })
    .await
    .map_err(|_| Problem::Setup)?
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            Problem::NotInstalled
        }
        _ => Problem::Setup,
    })
}

/// The exit's TUN, from the helper.
struct HelperExit;

impl ExitDevices for HelperExit {
    type Device = TunDevice;

    async fn open(&self, mtu: u16) -> std::io::Result<TunDevice> {
        match ask_helper(Request::OpenExit {
            link: ExitLink { mtu },
        })
        .await
        {
            Ok((Response::Opened { .. }, Some(fd))) => TunDevice::new(fd),
            Ok((answer, _)) => Err(std::io::Error::other(format!("{answer:?}"))),
            Err(problem) => Err(std::io::Error::other(format!("{problem:?}"))),
        }
    }

    async fn close(&self) {
        if let Err(problem) = ask_helper(Request::CloseExit).await {
            tracing::warn!(?problem, "could not close this device's exit");
        }
    }
}

/// What the client task and the prober learnt, for the status.
#[derive(Default)]
struct Shared {
    client: Option<(Want, ClientState)>,
    /// The person's devices that offer themselves as an exit.
    offered: Vec<[u8; 32]>,
}

/// What the client task and the prober follow.
#[derive(Clone, Default)]
struct Inputs {
    endpoint: Option<Endpoint>,
    want: Option<Want>,
    online: BTreeSet<[u8; 32]>,
}

impl Inputs {
    fn endpoint_id(&self) -> Option<EndpointId> {
        self.endpoint.as_ref().map(Endpoint::id)
    }
}

/// The fabric tick's memory between ticks.
#[derive(Default)]
struct Follow {
    settings_generation: Option<u64>,
    exit_on: bool,
    through: Option<Through>,
    stop: Option<StopRequest>,
    stop_done: Option<u64>,
    labels: BTreeMap<[u8; 32], String>,
    publisher: Option<(u32, u64)>,
    /// The status as of the last tick, for this process's own readers.
    current: Option<DeviceVpnStatus>,
    published: Option<DeviceVpnStatus>,
}

struct DeviceVpn {
    view: Arc<RwLock<View>>,
    exit: Exit<ShellAuthority, HelperExit>,
    shared: Arc<Mutex<Shared>>,
    inputs: watch::Sender<Inputs>,
    started: AtomicBool,
    follow: Mutex<Follow>,
}

static DEVICE_VPN: OnceLock<DeviceVpn> = OnceLock::new();
static LIVE_SETTINGS: OnceLock<settings_store::live::LiveSettings> = OnceLock::new();

fn device_vpn() -> &'static DeviceVpn {
    DEVICE_VPN.get_or_init(|| {
        let view = Arc::new(RwLock::new(View::default()));
        DeviceVpn {
            exit: Exit::new(
                ShellAuthority(Arc::clone(&view)),
                HelperExit,
                DnsRelay::system(),
                || {},
            ),
            view,
            shared: Arc::default(),
            inputs: watch::Sender::new(Inputs::default()),
            started: AtomicBool::new(false),
            follow: Mutex::default(),
        }
    })
}

/// Read this feature's settings from `settings` from now on, instead of
/// the settings document.
pub fn follow(settings: &settings_store::live::LiveSettings) {
    let _ = LIVE_SETTINGS.set(settings.clone());
}

/// Serve another device's tunnel connection until it ends.
pub async fn serve_connection(connection: iroh::endpoint::Connection) {
    device_vpn().exit.serve(connection).await;
}

fn key_of(device: &crate::shell::personal_continuity::DeviceId) -> Option<[u8; 32]> {
    let mut key = [0_u8; 32];
    hex::decode_to_slice(device.as_str(), &mut key).ok()?;
    Some(key)
}

fn unix_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

impl DeviceVpn {
    /// The settings, read again only when they changed.
    fn read_settings(follow: &mut Follow) {
        let Some(live) = LIVE_SETTINGS.get() else {
            return;
        };
        let generation = live.generation();
        if follow.settings_generation == Some(generation) {
            return;
        }
        follow.settings_generation = Some(generation);
        let section = live.section(settings::SECTION);
        follow.exit_on = settings::EXIT.from_section(&section);
        follow.through = settings::THROUGH.from_section(&section);
        follow.stop = settings::STOP.from_section(&section);
    }

    fn start(&self, network: &ShellNetwork) {
        if self.started.load(Ordering::Acquire) {
            return;
        }
        let Ok(handle) = network.runtime_handle() else {
            return;
        };
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        handle.spawn(drive_client(
            Arc::clone(&self.shared),
            self.inputs.subscribe(),
        ));
        handle.spawn(probe_exits(
            Arc::clone(&self.shared),
            self.inputs.subscribe(),
        ));
    }
}

/// Follow the settings and the person's devices, act on a stop, and publish
/// the status. Called from the fabric tick.
pub fn refresh(network: &ShellNetwork) {
    let vpn = device_vpn();
    vpn.start(network);
    let mut follow = vpn.follow.lock_recover();
    DeviceVpn::read_settings(&mut follow);

    let devices: HashSet<[u8; 32]> = network.personal_device_keys().into_iter().collect();
    let recheck = {
        let mut view = vpn.view.write_recover();
        let narrowed = view.devices.iter().any(|device| !devices.contains(device))
            || (view.exit_on && !follow.exit_on);
        view.devices = devices;
        view.exit_on = follow.exit_on;
        drop(view);
        narrowed
    };
    if recheck {
        vpn.exit.recheck();
    }

    if let Some(stop) = follow.stop.clone()
        && follow.stop_done != Some(stop.id)
    {
        follow.stop_done = Some(stop.id);
        stop_peer(&vpn.exit, &stop.device);
    }

    let fabric = network.device_fabric();
    let mut online = BTreeSet::new();
    follow.labels.clear();
    for snapshot in fabric.snapshots().values() {
        let presence = &snapshot.presence;
        if let Some(key) = key_of(&presence.device) {
            follow.labels.insert(key, presence.label.clone());
            if presence.online {
                online.insert(key);
            }
        }
    }
    let want = follow.through.as_ref().and_then(Want::of);
    let endpoint = network.peer_endpoint();
    vpn.inputs.send_if_modified(|inputs| {
        let changed = inputs.want != want
            || inputs.online != online
            || inputs.endpoint_id() != endpoint.as_ref().map(Endpoint::id);
        if changed {
            *inputs = Inputs {
                endpoint,
                want,
                online,
            };
        }
        changed
    });

    publish(vpn, &mut follow);
    drop(follow);
}

fn stop_peer(exit: &Exit<ShellAuthority, HelperExit>, device: &str) {
    let mut key = [0_u8; 32];
    if hex::decode_to_slice(device.trim(), &mut key).is_ok()
        && let Ok(peer) = EndpointId::from_bytes(&key)
    {
        exit.stop(&peer);
    }
}

fn label_of(labels: &BTreeMap<[u8; 32], String>, device: &[u8; 32]) -> String {
    labels.get(device).cloned().unwrap_or_default()
}

/// The status as it stands, without the publisher's identity.
fn status(vpn: &DeviceVpn, follow: &Follow) -> DeviceVpnStatus {
    let shared = vpn.shared.lock_recover();
    let client = shared.client.as_ref().map(|(want, state)| ClientStatus {
        device: hex::encode(want.device),
        label: label_of(&follow.labels, &want.device),
        state: state.clone(),
        routes: want.routes,
        kill_switch: want.kill_switch,
    });
    let offered = shared
        .offered
        .iter()
        .map(|device| OfferedExit {
            device: hex::encode(device),
            label: label_of(&follow.labels, device),
        })
        .collect();
    drop(shared);
    DeviceVpnStatus {
        exit_available: Path::new(protocol::SOCKET_PATH).exists(),
        exit_on: follow.exit_on,
        exit_users: vpn
            .exit
            .sessions()
            .into_iter()
            .map(|session| ExitUser {
                device: hex::encode(session.peer.as_bytes()),
                label: label_of(&follow.labels, session.peer.as_bytes()),
                since_unix_ms: unix_ms(session.since),
            })
            .collect(),
        offered,
        client,
        ..DeviceVpnStatus::default()
    }
}

/// Write the status for Settings when it changed, off this thread.
fn publish(vpn: &DeviceVpn, follow: &mut Follow) {
    let mut status = status(vpn, follow);
    follow.current = Some(status.clone());
    if follow.publisher.is_none() {
        follow.publisher = settings_store::hardware::publisher_identity().ok();
    }
    let Some((pid, ticks)) = follow.publisher else {
        return;
    };
    status.publisher_pid = pid;
    status.publisher_start_ticks = ticks;
    if follow.published.as_ref() == Some(&status) {
        return;
    }
    follow.published = Some(status.clone());
    let submitted = crate::shell::durable_writer::global().submit_ordered(move || {
        if let Err(error) = status.save() {
            tracing::warn!(%error, "could not publish the device VPN status");
        }
    });
    if let Err(error) = submitted {
        tracing::warn!(%error, "could not queue the device VPN status");
    }
}

/// The status this session last published, for quick settings and the
/// palette.
pub(crate) fn view() -> Option<DeviceVpnStatus> {
    DEVICE_VPN
        .get()
        .and_then(|vpn| vpn.follow.lock_recover().current.clone())
}

/// One of the routes as a quick settings VPN entry.
pub(crate) struct Entry {
    pub id: String,
    pub label: String,
    pub on: bool,
}

/// The routes quick settings lists after the system's VPN profiles: the
/// current one, then each device that offers itself.
pub(crate) fn entries() -> Vec<Entry> {
    view().map_or_else(Vec::new, |status| {
        status
            .rows()
            .into_iter()
            .map(|row| Entry {
                id: format!("{ENTRY_PREFIX}{}", row.device),
                on: row.is_on(),
                label: row.label,
            })
            .collect()
    })
}

/// The device a quick settings entry id names, if it is one of these.
pub(crate) fn entry_device(id: &str) -> Option<&str> {
    id.strip_prefix(ENTRY_PREFIX)
}

/// Route through `device` (hex), or stop routing through it.
///
/// # Errors
///
/// The change could not be queued.
pub(crate) fn choose(device: &str, on: bool) -> Result<(), String> {
    let device = device.to_owned();
    crate::shell::durable_writer::global()
        .submit_ordered(move || {
            let changed = settings::THROUGH.update(|through| {
                if on {
                    Some(Through::device(device))
                } else {
                    through.filter(|through| through.device != device)
                }
            });
            if let Err(error) = changed {
                tracing::warn!(%error, "could not change the device VPN route");
            }
        })
        .map_err(|error| error.to_string())
}

/// Stop `device` (hex) routing through this one.
pub(crate) fn stop_device(device: &str) {
    if let Some(vpn) = DEVICE_VPN.get() {
        stop_peer(&vpn.exit, device);
    }
}

/// Turn this device's exit on or off.
///
/// # Errors
///
/// The change could not be queued.
pub(crate) fn set_exit(on: bool) -> Result<(), String> {
    crate::shell::durable_writer::global()
        .submit_ordered(move || {
            if let Err(error) = settings::EXIT.set(on) {
                tracing::warn!(%error, "could not change whether this device shares its connection");
            }
        })
        .map_err(|error| error.to_string())
}

/// The client's link while the helper holds it.
struct Held {
    want: Want,
    link: ClientLink,
    device: Arc<TunDevice>,
}

fn set_state(shared: &Mutex<Shared>, want: &Want, state: ClientState) {
    shared.lock_recover().client = Some((want.clone(), state));
}

/// Resolves once the person's choice is no longer `want`, or this device's
/// endpoint changed. Never, once nobody can change it.
async fn choice_changes(inputs: &mut watch::Receiver<Inputs>, want: &Want, endpoint: EndpointId) {
    loop {
        if inputs.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        let now = inputs.borrow();
        if now.want.as_ref() != Some(want) || now.endpoint_id() != Some(endpoint) {
            return;
        }
    }
}

async fn release(held: &mut Option<Held>) {
    if held.take().is_some() {
        close_client_link().await;
    }
}

async fn close_client_link() {
    if let Err(problem) = ask_helper(Request::CloseClient).await {
        tracing::warn!(?problem, "could not undo this device's route");
    }
}

async fn open_link(link: &ClientLink) -> Result<TunDevice, Problem> {
    match ask_helper(Request::OpenClient { link: link.clone() }).await? {
        (Response::Opened { .. }, Some(fd)) => TunDevice::new(fd).map_err(|_| Problem::Setup),
        (answer, _) => {
            tracing::warn!(?answer, "the helper did not open this device's route");
            Err(Problem::Setup)
        }
    }
}

/// The relays' addresses, kept from the last lookup that found any: a
/// lookup while the kill switch blocks finds none.
static RELAYS: Mutex<Vec<IpAddr>> = Mutex::new(Vec::new());

async fn relay_addresses() -> Vec<IpAddr> {
    let urls: Vec<iroh::RelayUrl> = iroh::endpoint::default_relay_mode().relay_map().urls();
    let mut found = Vec::new();
    for url in urls {
        let Some(host) = url.host_str() else { continue };
        if let Ok(addresses) = tokio::net::lookup_host((host, 443)).await {
            found.extend(
                addresses
                    .map(|address| address.ip())
                    .filter(|ip| is_relay_address(*ip)),
            );
        }
    }
    found.sort_unstable();
    found.dedup();
    found.truncate(MAX_RELAYS);
    let mut kept = RELAYS.lock_recover();
    if !found.is_empty() {
        kept.clone_from(&found);
    }
    kept.clone()
}

/// What must reach the exit outside the tunnel: this endpoint's sockets and
/// the relays.
async fn carrier(endpoint: &Endpoint) -> Carrier {
    let mut udp_ports: Vec<u16> = endpoint
        .bound_sockets()
        .iter()
        .map(SocketAddr::port)
        .filter(|port| *port != 0)
        .collect();
    udp_ports.sort_unstable();
    udp_ports.dedup();
    udp_ports.truncate(MAX_CARRIER_PORTS);
    Carrier {
        udp_ports,
        relays: relay_addresses().await,
    }
}

/// Clear the person's choice of `want`'s device: that device stopped this
/// one.
fn clear_through(want: &Want) {
    let device = hex::encode(want.device);
    let submitted = crate::shell::durable_writer::global().submit_ordered(move || {
        let cleared = settings::THROUGH.update(|through| through.filter(|t| t.device != device));
        if let Err(error) = cleared {
            tracing::warn!(%error, "could not clear the device VPN route");
        }
    });
    if let Err(error) = submitted {
        tracing::warn!(%error, "could not queue clearing the device VPN route");
    }
}

/// Do what `after` says, then wait as long as it says or until the choice
/// changes.
async fn settle(
    shared: &Mutex<Shared>,
    held: &mut Option<Held>,
    want: &Want,
    endpoint: EndpointId,
    after: AfterEnd,
    inputs: &mut watch::Receiver<Inputs>,
) {
    if !after.keep_link {
        release(held).await;
    }
    if after.clear_through {
        clear_through(want);
    }
    set_state(shared, want, after.state);
    match after.retry {
        Some(delay) => {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = choice_changes(inputs, want, endpoint) => {}
            }
        }
        None => choice_changes(inputs, want, endpoint).await,
    }
}

/// The helper's link for `tunnel`: the one held when it still fits, else a
/// new one. On failure, what to do instead.
async fn link_up(
    tunnel: &Tunnel,
    endpoint: &Endpoint,
    want: &Want,
    held: &mut Option<Held>,
    attempt: u32,
) -> Result<Arc<TunDevice>, AfterEnd> {
    let keep_link = held.is_some() && want.kill_switch;
    let carrier = carrier(endpoint).await;
    let link = controller::link_for(want, tunnel.address, tunnel.mtu(), &tunnel.offer, carrier)
        .map_err(|reason| {
            tracing::info!(%reason, "no route through the other device");
            AfterEnd {
                keep_link,
                clear_through: false,
                retry: Some(controller::REFUSED_RETRY),
                state: ClientState::Failed {
                    problem: Problem::NoHomeNetwork,
                },
            }
        })?;
    if let Some(held) = held
        .as_ref()
        .filter(|held| held.link == link && held.want == *want)
    {
        return Ok(Arc::clone(&held.device));
    }
    let device = open_link(&link).await.map_err(|problem| AfterEnd {
        keep_link,
        clear_through: false,
        retry: Some(controller::backoff(attempt)),
        state: ClientState::Failed { problem },
    })?;
    let device = Arc::new(device);
    *held = Some(Held {
        want: want.clone(),
        link,
        device: Arc::clone(&device),
    });
    Ok(device)
}

/// Route through the device the person chose, for as long as they choose
/// it.
async fn drive_client(shared: Arc<Mutex<Shared>>, mut inputs: watch::Receiver<Inputs>) {
    let mut held: Option<Held> = None;
    let mut attempt: u32 = 0;
    // A route a crashed session left with its kill switch: undone unless the
    // person still wants it, when the new link replaces it.
    let mut stale = Path::new("/sys/class/net").join(CLIENT_INTERFACE).exists();
    loop {
        let current = inputs.borrow_and_update().clone();
        if controller::link_must_go(held.as_ref().map(|held| &held.want), current.want.as_ref()) {
            release(&mut held).await;
        }
        let Some(want) = current.want.clone() else {
            if std::mem::take(&mut stale) {
                close_client_link().await;
            }
            shared.lock_recover().client = None;
            attempt = 0;
            if inputs.changed().await.is_err() {
                return;
            }
            continue;
        };
        let (Some(endpoint), Ok(exit)) = (
            current.endpoint.clone(),
            EndpointId::from_bytes(&want.device),
        ) else {
            if inputs.changed().await.is_err() {
                return;
            }
            continue;
        };
        stale = false;
        let endpoint_id = endpoint.id();
        attempt = attempt.saturating_add(1);
        let blocking = held.is_some() && want.kill_switch;
        set_state(
            &shared,
            &want,
            if held.is_some() {
                ClientState::Reconnecting { blocking }
            } else {
                ClientState::Connecting
            },
        );
        let dialled = tokio::select! {
            result = client::connect(&endpoint, EndpointAddr::new(exit)) => Some(result),
            () = choice_changes(&mut inputs, &want, endpoint_id) => None,
        };
        let Some(dialled) = dialled else { continue };
        let tunnel = match dialled {
            Ok(tunnel) => tunnel,
            Err(error) => {
                let after = controller::after_failed_dial(
                    &error,
                    held.is_some(),
                    want.kill_switch,
                    attempt,
                );
                settle(&shared, &mut held, &want, endpoint_id, after, &mut inputs).await;
                continue;
            }
        };
        let device = match link_up(&tunnel, &endpoint, &want, &mut held, attempt).await {
            Ok(device) => device,
            Err(after) => {
                tunnel.close();
                settle(&shared, &mut held, &want, endpoint_id, after, &mut inputs).await;
                continue;
            }
        };
        attempt = 0;
        set_state(
            &shared,
            &want,
            ClientState::Connected {
                since_unix_ms: unix_ms(SystemTime::now()),
            },
        );
        let ended = tokio::select! {
            ended = client::run(&tunnel, device) => ended,
            () = choice_changes(&mut inputs, &want, endpoint_id) => {
                tunnel.close();
                Ended::Closed
            }
        };
        tracing::info!(?ended, "the route through another device ended");
        let after = controller::after_end(&ended, want.kill_switch, 1);
        settle(&shared, &mut held, &want, endpoint_id, after, &mut inputs).await;
    }
}

/// Learn which of the person's online devices offer themselves: when the
/// online devices change, and every [`PROBE_EVERY`].
async fn probe_exits(shared: Arc<Mutex<Shared>>, mut inputs: watch::Receiver<Inputs>) {
    let mut asked: Option<(BTreeSet<[u8; 32]>, Instant)> = None;
    loop {
        let current = inputs.borrow_and_update().clone();
        let due = asked
            .as_ref()
            .is_none_or(|(online, at)| *online != current.online || at.elapsed() >= PROBE_EVERY);
        if due && let Some(endpoint) = &current.endpoint {
            let mut offered = Vec::new();
            for device in &current.online {
                let Ok(id) = EndpointId::from_bytes(device) else {
                    continue;
                };
                if client::probe(endpoint, EndpointAddr::new(id)).await.is_ok() {
                    offered.push(*device);
                }
            }
            shared.lock_recover().offered = offered;
            asked = Some((current.online.clone(), Instant::now()));
        }
        tokio::select! {
            changed = inputs.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = tokio::time::sleep(PROBE_EVERY) => {}
        }
    }
}
